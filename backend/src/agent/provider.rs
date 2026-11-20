//! Multi-Provider LLM integration backed directly by `agent-kernel` (`xai-grok-sampler` + `xai-grok-sampling-types`).
//!
//! Routes Chat and Memory requests through [`xai_grok_sampler::MultiProviderRouter`] (`SamplerActor`
//! + `Eventsource` SSE streaming `/chat/completions`, `/responses`, `/messages` [Anthropic Prompt Caching],
//! and `:streamGenerateContent?alt=sse` [Google Gemini]) with automatic failover and real-time token deltas.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::env;
use std::sync::Arc;

use xai_grok_sampler::{
    ModelRole as KernelModelRole, MultiProviderRouter, ProviderEndpoint, SamplingChannel,
    SamplingEvent,
};
use xai_grok_sampling_types::{
    ApiBackend, AssistantItem, ConversationItem, ConversationRequest, ToolCall, ToolResultItem,
    ToolSpec,
};

use crate::{state::AppState, time_bj::now_ms};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfigDto {
    pub id: String,
    pub name: String,
    /// `"openai_compat"` | `"responses"` | `"anthropic"` | `"gemini"`
    pub protocol: String,
    pub base_url: String,
    /// Masked on GET responses; raw key only stored server-side.
    pub api_key: String,
    pub chat_model: String,
    pub memory_model: String,
    pub is_chat_primary: bool,
    pub is_memory_primary: bool,
    pub enabled: bool,
    pub priority: i64,
    #[serde(default = "default_health_unknown")]
    pub health_status: String,
    #[serde(default)]
    pub health_error: Option<String>,
}

fn default_health_unknown() -> String {
    "unknown".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallItem {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum LlmMessage {
    User {
        content: String,
    },
    Assistant {
        #[serde(default)]
        content: String,
        #[serde(default)]
        tool_calls: Vec<ToolCallItem>,
    },
    Tool {
        tool_call_id: String,
        name: String,
        content: String,
    },
}

#[derive(Debug, Clone)]
pub struct ToolSpecWire {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct LlmCompletion {
    pub text: String,
    pub tool_calls: Vec<ToolCallItem>,
    pub provider_used: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    Chat,
    Memory,
}

impl From<ModelRole> for KernelModelRole {
    fn from(role: ModelRole) -> Self {
        match role {
            ModelRole::Chat => KernelModelRole::Chat,
            ModelRole::Memory => KernelModelRole::Memory,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VendorPreset {
    pub vendor_id: &'static str,
    pub display_name: &'static str,
    pub protocol: &'static str,
    pub base_url: &'static str,
    pub default_chat_model: &'static str,
    pub default_memory_model: &'static str,
}

pub const VENDOR_PRESETS: &[VendorPreset] = &[
    VendorPreset {
        vendor_id: "deepseek",
        display_name: "DeepSeek",
        protocol: "openai_compat",
        base_url: "https://api.deepseek.com/v1",
        default_chat_model: "deepseek-chat",
        default_memory_model: "deepseek-chat",
    },
    VendorPreset {
        vendor_id: "minimax",
        display_name: "MiniMax",
        protocol: "openai_compat",
        base_url: "https://api.minimaxi.com/v1",
        default_chat_model: "MiniMax-M2.7",
        default_memory_model: "MiniMax-M2.7",
    },
    VendorPreset {
        vendor_id: "kimi",
        display_name: "Kimi (月之暗面)",
        protocol: "openai_compat",
        base_url: "https://api.moonshot.cn/v1",
        default_chat_model: "kimi-k2-turbo-preview",
        default_memory_model: "moonshot-v1-auto",
    },
    VendorPreset {
        vendor_id: "zhipu",
        display_name: "智谱 GLM",
        protocol: "openai_compat",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        default_chat_model: "glm-4-plus",
        default_memory_model: "glm-4-flash",
    },
    VendorPreset {
        vendor_id: "qwen",
        display_name: "通义千问 Qwen",
        protocol: "openai_compat",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_chat_model: "qwen-plus",
        default_memory_model: "qwen-turbo",
    },
    VendorPreset {
        vendor_id: "doubao",
        display_name: "豆包 Doubao",
        protocol: "openai_compat",
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        default_chat_model: "doubao-1-5-pro-32k-250115",
        default_memory_model: "doubao-1-5-lite-32k-250115",
    },
    VendorPreset {
        vendor_id: "siliconflow",
        display_name: "硅基流动 SiliconFlow",
        protocol: "openai_compat",
        base_url: "https://api.siliconflow.cn/v1",
        default_chat_model: "deepseek-ai/DeepSeek-V3",
        default_memory_model: "Qwen/Qwen2.5-7B-Instruct",
    },
    VendorPreset {
        vendor_id: "openrouter",
        display_name: "OpenRouter",
        protocol: "openai_compat",
        base_url: "https://openrouter.ai/api/v1",
        default_chat_model: "anthropic/claude-3.7-sonnet",
        default_memory_model: "openai/gpt-4o-mini",
    },
    VendorPreset {
        vendor_id: "anthropic",
        display_name: "Claude (Anthropic)",
        protocol: "anthropic",
        base_url: "https://api.anthropic.com/v1",
        default_chat_model: "claude-sonnet-4-20250514",
        default_memory_model: "claude-3-5-haiku-latest",
    },
    VendorPreset {
        vendor_id: "openai",
        display_name: "OpenAI",
        protocol: "openai_compat",
        base_url: "https://api.openai.com/v1",
        default_chat_model: "gpt-4o",
        default_memory_model: "gpt-4o-mini",
    },
    VendorPreset {
        vendor_id: "gemini",
        display_name: "Google Gemini",
        protocol: "gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta",
        default_chat_model: "gemini-2.5-flash",
        default_memory_model: "gemini-2.5-flash",
    },
    VendorPreset {
        vendor_id: "xai",
        display_name: "xAI Grok",
        protocol: "openai_compat",
        base_url: "https://api.x.ai/v1",
        default_chat_model: "grok-3",
        default_memory_model: "grok-3-mini",
    },
];

pub fn resolve_vendor_preset(hint: &str) -> Option<&'static VendorPreset> {
    let lower = hint.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if let Some(exact) = VENDOR_PRESETS.iter().find(|p| p.vendor_id == lower) {
        return Some(exact);
    }
    if lower.contains("minimax") || lower.contains("稀宇") {
        return resolve_vendor_preset("minimax");
    }
    if lower.contains("deepseek") || lower.contains("深度求索") {
        return resolve_vendor_preset("deepseek");
    }
    if lower.contains("kimi") || lower.contains("moonshot") || lower.contains("月之暗面") {
        return resolve_vendor_preset("kimi");
    }
    if lower.contains("zhipu") || lower.contains("glm") || lower.contains("bigmodel") || hint.contains("智谱") {
        return resolve_vendor_preset("zhipu");
    }
    if lower.contains("qwen") || lower.contains("dashscope") || hint.contains("千问") || hint.contains("通义") {
        return resolve_vendor_preset("qwen");
    }
    if lower.contains("doubao") || lower.contains("volc") || lower.contains("ark") || hint.contains("豆包") {
        return resolve_vendor_preset("doubao");
    }
    if lower.contains("silicon") || hint.contains("硅基") {
        return resolve_vendor_preset("siliconflow");
    }
    if lower.contains("openrouter") {
        return resolve_vendor_preset("openrouter");
    }
    if lower.contains("claude") || lower.contains("anthropic") {
        return resolve_vendor_preset("anthropic");
    }
    if lower.contains("gemini") || lower.contains("google") {
        return resolve_vendor_preset("gemini");
    }
    if lower.contains("grok") || lower.contains("xai") || lower.contains("x.ai") {
        return resolve_vendor_preset("xai");
    }
    if lower.contains("openai") || lower.contains("gpt") {
        return resolve_vendor_preset("openai");
    }
    None
}

async fn query_providers_internal(
    state: &AppState,
    space_id: i64,
    only_enabled: bool,
) -> Vec<ProviderConfigDto> {
    let mut out = Vec::new();
    let sql = if only_enabled {
        "SELECT id, name, protocol, base_url, api_key, chat_model, memory_model,
                is_chat_primary, is_memory_primary, enabled, priority
         FROM agent_providers
         WHERE space_id IN (?1, 0) AND enabled = 1
         ORDER BY CASE WHEN space_id = ?1 THEN 0 ELSE 1 END, is_chat_primary DESC, priority ASC, updated_at DESC"
    } else {
        "SELECT id, name, protocol, base_url, api_key, chat_model, memory_model,
                is_chat_primary, is_memory_primary, enabled, priority
         FROM agent_providers
         WHERE space_id IN (?1, 0)
         ORDER BY CASE WHEN space_id = ?1 THEN 0 ELSE 1 END, is_chat_primary DESC, enabled DESC, priority ASC, updated_at DESC"
    };

    if let Ok(rows) = sqlx::query(sql)
        .bind(space_id)
        .fetch_all(&state.db.read)
        .await
    {
        for r in rows {
            out.push(ProviderConfigDto {
                id: r.get("id"),
                name: r.get("name"),
                protocol: r.get("protocol"),
                base_url: r.get("base_url"),
                api_key: r.get("api_key"),
                chat_model: r.get("chat_model"),
                memory_model: r.get("memory_model"),
                is_chat_primary: r.get::<i64, _>("is_chat_primary") != 0,
                is_memory_primary: r.get::<i64, _>("is_memory_primary") != 0,
                enabled: r.get::<i64, _>("enabled") != 0,
                priority: r.get("priority"),
                health_status: "unknown".to_string(),
                health_error: None,
            });
        }
    }

    let env_presets = [
        (
            "env_deepseek",
            "DeepSeek (Env)",
            "openai_compat",
            "DEEPSEEK_API_KEY",
            "DEEPSEEK_BASE_URL",
            "https://api.deepseek.com/v1",
            "DEEPSEEK_MODEL",
            "deepseek-chat",
            "deepseek-chat",
        ),
        (
            "env_minimax",
            "MiniMax (Env)",
            "openai_compat",
            "MINIMAX_API_KEY",
            "MINIMAX_BASE_URL",
            "https://api.minimaxi.com/v1",
            "MINIMAX_MODEL",
            "MiniMax-M2.7",
            "MiniMax-M2.7",
        ),
        (
            "env_kimi",
            "Kimi (Env)",
            "openai_compat",
            "MOONSHOT_API_KEY",
            "MOONSHOT_BASE_URL",
            "https://api.moonshot.cn/v1",
            "MOONSHOT_MODEL",
            "kimi-k2-turbo-preview",
            "moonshot-v1-auto",
        ),
        (
            "env_zhipu",
            "智谱 GLM (Env)",
            "openai_compat",
            "ZHIPU_API_KEY",
            "ZHIPU_BASE_URL",
            "https://open.bigmodel.cn/api/paas/v4",
            "ZHIPU_MODEL",
            "glm-4-plus",
            "glm-4-flash",
        ),
        (
            "env_qwen",
            "通义千问 Qwen (Env)",
            "openai_compat",
            "DASHSCOPE_API_KEY",
            "DASHSCOPE_BASE_URL",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "DASHSCOPE_MODEL",
            "qwen-plus",
            "qwen-turbo",
        ),
        (
            "env_siliconflow",
            "SiliconFlow (Env)",
            "openai_compat",
            "SILICONFLOW_API_KEY",
            "SILICONFLOW_BASE_URL",
            "https://api.siliconflow.cn/v1",
            "SILICONFLOW_MODEL",
            "deepseek-ai/DeepSeek-V3",
            "Qwen/Qwen2.5-7B-Instruct",
        ),
        (
            "env_openrouter",
            "OpenRouter (Env)",
            "openai_compat",
            "OPENROUTER_API_KEY",
            "OPENROUTER_BASE_URL",
            "https://openrouter.ai/api/v1",
            "OPENROUTER_MODEL",
            "anthropic/claude-3.7-sonnet",
            "openai/gpt-4o-mini",
        ),
        (
            "env_openai",
            "OpenAI (Env)",
            "openai_compat",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "https://api.openai.com/v1",
            "OPENAI_MODEL",
            "gpt-4o",
            "gpt-4o-mini",
        ),
        (
            "env_xai",
            "xAI Grok (Env)",
            "openai_compat",
            "XAI_API_KEY",
            "XAI_BASE_URL",
            "https://api.x.ai/v1",
            "XAI_MODEL",
            "grok-3",
            "grok-3-mini",
        ),
        (
            "env_anthropic",
            "Claude (Env)",
            "anthropic",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "https://api.anthropic.com/v1",
            "ANTHROPIC_MODEL",
            "claude-sonnet-4-20250514",
            "claude-3-5-haiku-latest",
        ),
        (
            "env_gemini",
            "Google Gemini (Env)",
            "gemini",
            "GEMINI_API_KEY",
            "GEMINI_BASE_URL",
            "https://generativelanguage.googleapis.com/v1beta",
            "GEMINI_MODEL",
            "gemini-2.5-flash",
            "gemini-2.5-flash",
        ),
    ];

    for (id, name, proto, key_var, url_var, def_url, model_var, def_chat, def_mem) in env_presets {
        if let Ok(k) = env::var(key_var) {
            let trimmed = k.trim();
            if !trimmed.is_empty() && !out.iter().any(|p| p.id == id) {
                let base_url = env::var(url_var).unwrap_or_else(|_| def_url.to_string());
                let chat_model = env::var(model_var).unwrap_or_else(|_| def_chat.to_string());
                let memory_model = env::var("AGENT_MEMORY_MODEL")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| def_mem.to_string());
                out.push(ProviderConfigDto {
                    id: id.to_string(),
                    name: name.to_string(),
                    protocol: proto.to_string(),
                    base_url,
                    api_key: trimmed.to_string(),
                    chat_model,
                    memory_model,
                    is_chat_primary: out.is_empty(),
                    is_memory_primary: out.is_empty(),
                    enabled: true,
                    priority: 200,
                    health_status: "unknown".to_string(),
                    health_error: None,
                });
            }
        }
    }

    for p in &mut out {
        if let Some(raw_h) = state.cache.get_str(&format!("prov:health:{}", p.id)).await {
            if let Ok(val) = serde_json::from_str::<Value>(&raw_h) {
                if let Some(st) = val.get("status").and_then(|v| v.as_str()) {
                    p.health_status = st.to_string();
                }
                p.health_error = val
                    .get("error")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
            }
        }
    }

    out
}

pub async fn set_cached_provider_health(
    state: &AppState,
    provider_id: &str,
    status: &str,
    error: Option<&str>,
) {
    let base_id = provider_id.strip_suffix("_mirror").unwrap_or(provider_id);
    let payload = json!({
        "status": status,
        "error": error.unwrap_or(""),
        "at": now_ms(),
    });
    if let Ok(s) = serde_json::to_string(&payload) {
        state
            .cache
            .set_str(&format!("prov:health:{base_id}"), &s, 86_400)
            .await;
    }
}

async fn ping_single_endpoint(
    client: &reqwest::Client,
    protocol: &str,
    raw_base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<(), (u16, String)> {
    let base = normalize_provider_base_url(protocol, raw_base_url);
    let resp = match protocol {
        "anthropic" => {
            client
                .post(format!("{base}/messages"))
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&json!({
                    "model": model,
                    "max_tokens": 1,
                    "messages": [{ "role": "user", "content": "hi" }]
                }))
                .send()
                .await
        }
        "gemini" => {
            client
                .post(format!(
                    "{base}/models/{model}:generateContent?key={api_key}"
                ))
                .json(&json!({
                    "contents": [{ "parts": [{ "text": "hi" }] }],
                    "generationConfig": { "maxOutputTokens": 1 }
                }))
                .send()
                .await
        }
        _ => {
            client
                .post(format!("{base}/chat/completions"))
                .bearer_auth(api_key)
                .json(&json!({
                    "model": model,
                    "max_tokens": 1,
                    "messages": [{ "role": "user", "content": "hi" }]
                }))
                .send()
                .await
        }
    }
    .map_err(|e| {
        if e.is_timeout() {
            (0u16, "连接供应商网关超时".to_string())
        } else {
            (0u16, "无法连接模型网关，请检查网络".to_string())
        }
    })?;

    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        // Also check MiniMax / domestic JSON-level `base_resp.status_code != 0` inside HTTP 200!
        if let Ok(body_val) = resp.json::<Value>().await {
            if let Some(code) = body_val
                .get("base_resp")
                .and_then(|b| b.get("status_code"))
                .and_then(|c| c.as_i64())
            {
                if code != 0 {
                    let msg = body_val
                        .get("base_resp")
                        .and_then(|b| b.get("status_msg"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("API Token 校验未通过");
                    return Err((401, format!("未连通：{msg}")));
                }
            }
            if let Some(err_obj) = body_val.get("error") {
                let msg = err_obj
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("上游接口返回错误");
                return Err((400, format!("未连通：{msg}")));
            }
        }
        return Ok(());
    }

    let raw_body = resp.text().await.unwrap_or_default();
    let lower_body = raw_body.to_ascii_lowercase();
    let friendly = if status == 401 || status == 403 {
        "API Token 无效或未授权".to_string()
    } else if status == 402
        || lower_body.contains("insufficient")
        || lower_body.contains("quota")
        || lower_body.contains("balance")
        || raw_body.contains("余额")
    {
        "账户余额或调用额度不足".to_string()
    } else if status == 404 || (lower_body.contains("model") && lower_body.contains("not")) {
        format!("模型 {model} 不存在或未开通权限")
    } else if status == 429 {
        // Note: 429 with valid auth means the key itself is valid, unless quota is 0!
        "请求频率受限或额度已满 (429)".to_string()
    } else {
        format!("网关响应异常 (HTTP {status})")
    };

    Err((status, friendly))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveModelOption {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchLiveModelsResult {
    pub connected: bool,
    pub error: Option<String>,
    pub resolved_base_url: String,
    pub models: Vec<LiveModelOption>,
}

fn is_non_chat_model_id(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    l.contains("embedding")
        || l.contains("embed")
        || l.contains("rerank")
        || l.contains("moderation")
        || l.contains("whisper")
        || l.contains("tts")
        || l.contains("speech")
        || l.contains("asr")
        || l.contains("dall-e")
        || l.contains("imagen")
        || l.starts_with("image-")
        || l.contains("hailuo")
        || l.contains("video")
        || l.starts_with("babbage")
        || l.starts_with("davinci")
        || l.contains("aqa")
        || l.contains("realtime")
        || l.contains("transcribe")
}

async fn fetch_models_from_single_endpoint(
    client: &reqwest::Client,
    protocol: &str,
    raw_base_url: &str,
    api_key: &str,
) -> Result<Vec<LiveModelOption>, (u16, String)> {
    let base = normalize_provider_base_url(protocol, raw_base_url);
    let resp = match protocol {
        "anthropic" => {
            client
                .get(format!("{base}/models"))
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
        }
        "gemini" => {
            client
                .get(format!("{base}/models?key={api_key}&pageSize=100"))
                .send()
                .await
        }
        _ => {
            client
                .get(format!("{base}/models"))
                .bearer_auth(api_key)
                .send()
                .await
        }
    }
    .map_err(|e| {
        if e.is_timeout() {
            (0u16, "连接供应商接口超时".to_string())
        } else {
            (0u16, "无法连接供应商网关，请检查网络".to_string())
        }
    })?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let raw_body = resp.text().await.unwrap_or_default();
        let lower_body = raw_body.to_ascii_lowercase();
        let friendly = if status == 401 || status == 403 {
            "API Token 无效或未授权".to_string()
        } else if status == 402
            || lower_body.contains("insufficient")
            || lower_body.contains("quota")
            || lower_body.contains("balance")
            || raw_body.contains("余额")
        {
            "账户余额或调用额度不足".to_string()
        } else if status == 429 {
            "请求频率受限 (429)".to_string()
        } else {
            format!("接口响应异常 (HTTP {status})")
        };
        return Err((status, friendly));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|_| (502u16, "解析官方模型列表失败".to_string()))?;

    // Check domestic JSON-level error envelope inside HTTP 200
    if let Some(code) = body
        .get("base_resp")
        .and_then(|b| b.get("status_code"))
        .and_then(|c| c.as_i64())
    {
        if code != 0 {
            let msg = body
                .get("base_resp")
                .and_then(|b| b.get("status_msg"))
                .and_then(|m| m.as_str())
                .unwrap_or("API Token 校验未通过");
            return Err((401, msg.to_string()));
        }
    }

    let mut parsed: Vec<(i64, LiveModelOption)> = Vec::new();

    if protocol == "gemini" {
        if let Some(arr) = body.get("models").and_then(|m| m.as_array()) {
            for item in arr {
                let supports_gen = item
                    .get("supportedGenerationMethods")
                    .and_then(|m| m.as_array())
                    .map(|methods| {
                        methods
                            .iter()
                            .any(|x| x.as_str() == Some("generateContent"))
                    })
                    .unwrap_or(true);
                if !supports_gen {
                    continue;
                }
                let raw_name = item.get("name").and_then(|s| s.as_str()).unwrap_or("");
                let clean_id = raw_name.strip_prefix("models/").unwrap_or(raw_name).trim();
                if clean_id.is_empty() || is_non_chat_model_id(clean_id) {
                    continue;
                }
                let label = item
                    .get("displayName")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(|d| format!("{d} ({clean_id})"))
                    .unwrap_or_else(|| clean_id.to_string());
                let rank = if clean_id.contains("2.5") {
                    300
                } else if clean_id.contains("2.0") {
                    200
                } else {
                    100
                };
                parsed.push((
                    rank,
                    LiveModelOption {
                        id: clean_id.to_string(),
                        label,
                    },
                ));
            }
        }
    } else if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            let id = item
                .get("id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim();
            if id.is_empty() || is_non_chat_model_id(id) {
                continue;
            }
            let disp = item
                .get("display_name")
                .or_else(|| item.get("name"))
                .and_then(|s| s.as_str())
                .filter(|s| !s.trim().is_empty() && *s != id);
            let label = match disp {
                Some(d) => format!("{d} ({id})"),
                None => id.to_string(),
            };
            let created = item.get("created").and_then(|c| c.as_i64()).unwrap_or(0);
            parsed.push((
                created,
                LiveModelOption {
                    id: id.to_string(),
                    label,
                },
            ));
        }
    }

    // Deduplicate & sort newest first (cap at 80 for aggregators like OpenRouter)
    parsed.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::new();
    for (_, opt) in parsed {
        if seen.insert(opt.id.clone()) {
            models.push(opt);
            if models.len() >= 80 {
                break;
            }
        }
    }

    Ok(models)
}

/// Fetches live available chat models directly from the official vendor `/models` API
/// (with automatic CN <-> Global mirror detection).
pub async fn fetch_live_vendor_models(
    state: &AppState,
    space_id: i64,
    vendor_hint: &str,
    provider_id: Option<&str>,
    raw_api_key: Option<&str>,
) -> FetchLiveModelsResult {
    let preset = resolve_vendor_preset(vendor_hint);
    let saved_all = query_providers_internal(state, space_id, false).await;
    let saved = provider_id
        .and_then(|pid| saved_all.iter().find(|p| p.id == pid))
        .or_else(|| {
            preset.and_then(|pr| {
                saved_all
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(pr.display_name) || p.name.eq_ignore_ascii_case(pr.vendor_id))
            })
        })
        .or_else(|| {
            saved_all
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(vendor_hint))
        });

    let protocol = saved
        .map(|s| s.protocol.as_str())
        .or_else(|| preset.map(|p| p.protocol))
        .unwrap_or("openai_compat");
    let primary_base_url = saved
        .map(|s| s.base_url.clone())
        .or_else(|| preset.map(|p| p.base_url.to_string()))
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let fallback_model = saved
        .map(|s| s.chat_model.clone())
        .or_else(|| preset.map(|p| p.default_chat_model.to_string()))
        .unwrap_or_else(|| "gpt-4o".to_string());

    let effective_key = match raw_api_key
        .map(str::trim)
        .filter(|k| !k.is_empty() && !k.contains("••••"))
    {
        Some(k) => k.to_string(),
        None => saved.map(|s| s.api_key.clone()).unwrap_or_default(),
    };

    if effective_key.trim().is_empty() {
        return FetchLiveModelsResult {
            connected: false,
            error: Some("请先填入 API Token 以拉取官方可用模型".to_string()),
            resolved_base_url: primary_base_url,
            models: Vec::new(),
        };
    }

    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
    else {
        return FetchLiveModelsResult {
            connected: false,
            error: Some("初始化网络客户端失败".to_string()),
            resolved_base_url: primary_base_url,
            models: Vec::new(),
        };
    };

    let mut urls_to_try = vec![primary_base_url.clone()];
    if let Some(mirror) = mirror_region_base_url(&primary_base_url) {
        urls_to_try.push(mirror.to_string());
    }

    let mut last_err = (0u16, "未连通".to_string());
    for candidate_url in &urls_to_try {
        match fetch_models_from_single_endpoint(
            &client,
            protocol,
            candidate_url,
            effective_key.trim(),
        )
        .await
        {
            Ok(mut models) => {
                if models.is_empty() {
                    models.push(LiveModelOption {
                        id: fallback_model.clone(),
                        label: fallback_model.clone(),
                    });
                }
                if let Some(s) = saved {
                    set_cached_provider_health(state, &s.id, "connected", None).await;
                    if candidate_url != &s.base_url && !s.id.starts_with("env_") {
                        let _ = sqlx::query(
                            "UPDATE agent_providers SET base_url = ?1 WHERE id = ?2 AND space_id = ?3",
                        )
                        .bind(candidate_url)
                        .bind(&s.id)
                        .bind(space_id)
                        .execute(&state.db.write)
                        .await;
                    }
                }
                return FetchLiveModelsResult {
                    connected: true,
                    error: None,
                    resolved_base_url: candidate_url.clone(),
                    models,
                };
            }
            Err((status, msg)) => {
                // If endpoint doesn't expose `/models` (404), fall back to 1-token chat ping
                if status == 404 {
                    if ping_single_endpoint(
                        &client,
                        protocol,
                        candidate_url,
                        effective_key.trim(),
                        &fallback_model,
                    )
                    .await
                    .is_ok()
                    {
                        if let Some(s) = saved {
                            set_cached_provider_health(state, &s.id, "connected", None).await;
                        }
                        return FetchLiveModelsResult {
                            connected: true,
                            error: None,
                            resolved_base_url: candidate_url.clone(),
                            models: vec![LiveModelOption {
                                id: fallback_model.clone(),
                                label: fallback_model,
                            }],
                        };
                    }
                }
                last_err = (status, msg);
            }
        }
    }

    if let Some(s) = saved {
        set_cached_provider_health(state, &s.id, "error", Some(&last_err.1)).await;
    }

    FetchLiveModelsResult {
        connected: false,
        error: Some(last_err.1),
        resolved_base_url: primary_base_url,
        models: Vec::new(),
    }
}

/// Actively probes a configured provider's connectivity (including CN <-> Global mirror auto-healing)
/// and caches `connected` or `error` + human-readable explanation.
pub async fn probe_and_cache_provider(state: &AppState, space_id: i64, provider_id: &str) {
    let all = query_providers_internal(state, space_id, false).await;
    let Some(target) = all.into_iter().find(|p| p.id == provider_id) else {
        return;
    };
    let _ = fetch_live_vendor_models(
        state,
        space_id,
        &target.name,
        Some(provider_id),
        Some(&target.api_key),
    )
    .await;
}

/// Loads enabled providers for live inference routing (`enabled = 1`).
pub async fn load_effective_providers(state: &AppState, space_id: i64) -> Vec<ProviderConfigDto> {
    query_providers_internal(state, space_id, true).await
}

/// Lists all saved providers for CC-Switch UI management (including temporarily paused ones).
pub async fn list_configured_providers(state: &AppState, space_id: i64) -> Vec<ProviderConfigDto> {
    query_providers_internal(state, space_id, false).await
}

fn normalize_provider_base_url(protocol: &str, raw_url: &str) -> String {
    let trimmed = raw_url.trim().trim_end_matches('/');
    match protocol {
        "anthropic" => {
            if trimmed.ends_with("/v1") {
                trimmed.to_string()
            } else {
                format!("{trimmed}/v1")
            }
        }
        "gemini" => {
            if trimmed.ends_with("/v1beta") || trimmed.ends_with("/v1") {
                trimmed.to_string()
            } else {
                format!("{trimmed}/v1beta")
            }
        }
        _ => trimmed.to_string(),
    }
}

fn to_kernel_endpoint(p: &ProviderConfigDto, role: ModelRole) -> Option<ProviderEndpoint> {
    if p.api_key.trim().is_empty() || p.chat_model.trim().is_empty() {
        return None;
    }
    let api_backend = match p.protocol.as_str() {
        "anthropic" => ApiBackend::Messages,
        "gemini" => ApiBackend::Gemini,
        "responses" => ApiBackend::Responses,
        _ => ApiBackend::ChatCompletions,
    };
    let (max_tokens, temp) = match role {
        ModelRole::Chat => (Some(4096), Some(0.55)),
        ModelRole::Memory => (Some(1536), Some(0.15)),
    };
    Some(ProviderEndpoint {
        id: p.id.clone(),
        label: p.name.clone(),
        api_backend,
        base_url: normalize_provider_base_url(&p.protocol, &p.base_url),
        api_key: p.api_key.trim().to_string(),
        chat_model: p.chat_model.trim().to_string(),
        memory_model: (!p.memory_model.trim().is_empty()).then(|| p.memory_model.trim().to_string()),
        extra_headers: IndexMap::new(),
        max_completion_tokens: max_tokens,
        temperature: temp,
    })
}

fn mirror_region_base_url(base_url: &str) -> Option<&'static str> {
    if base_url.contains("api.minimaxi.com") {
        Some("https://api.minimax.io/v1")
    } else if base_url.contains("api.minimax.io") {
        Some("https://api.minimaxi.com/v1")
    } else if base_url.contains("api.moonshot.cn") {
        Some("https://api.moonshot.ai/v1")
    } else if base_url.contains("api.moonshot.ai") {
        Some("https://api.moonshot.cn/v1")
    } else if base_url.contains("open.bigmodel.cn") {
        Some("https://api.z.ai/api/paas/v4")
    } else if base_url.contains("api.z.ai") {
        Some("https://open.bigmodel.cn/api/paas/v4")
    } else if base_url.contains("dashscope.aliyuncs.com") {
        Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1")
    } else if base_url.contains("dashscope-intl.aliyuncs.com") {
        Some("https://dashscope.aliyuncs.com/compatible-mode/v1")
    } else if base_url.contains("api.siliconflow.cn") {
        Some("https://api.siliconflow.com/v1")
    } else if base_url.contains("api.siliconflow.com") {
        Some("https://api.siliconflow.cn/v1")
    } else {
        None
    }
}

fn expand_kernel_endpoints(p: &ProviderConfigDto, role: ModelRole) -> Vec<ProviderEndpoint> {
    let Some(primary) = to_kernel_endpoint(p, role) else {
        return Vec::new();
    };
    let mut list = vec![primary.clone()];
    if let Some(mirror) = mirror_region_base_url(&primary.base_url) {
        let mut alt = primary;
        alt.id = format!("{}_mirror", alt.id);
        alt.base_url = normalize_provider_base_url(&p.protocol, mirror);
        list.push(alt);
    }
    list
}

fn build_kernel_conversation_request(
    system_static: &str,
    ltm_block: &str,
    messages: &[LlmMessage],
    tools: &[ToolSpecWire],
) -> ConversationRequest {
    let mut items: Vec<ConversationItem> = Vec::with_capacity(messages.len() + 2);
    // Static System Prompt followed immediately by Long-Term Memory HEAD -> maximizes Prefix KV-Cache hits!
    if !system_static.trim().is_empty() {
        items.push(ConversationItem::system(system_static));
    }
    if !ltm_block.trim().is_empty() {
        items.push(ConversationItem::system(ltm_block));
    }

    for msg in messages {
        match msg {
            LlmMessage::User { content } => {
                items.push(ConversationItem::user(content.as_str()));
            }
            LlmMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mapped_calls: Vec<ToolCall> = tool_calls
                    .iter()
                    .map(|tc| ToolCall {
                        id: Arc::<str>::from(tc.id.as_str()),
                        name: tc.name.clone(),
                        arguments: Arc::<str>::from(
                            serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()),
                        ),
                    })
                    .collect();
                items.push(ConversationItem::Assistant(AssistantItem {
                    content: Arc::<str>::from(content.as_str()),
                    tool_calls: mapped_calls,
                    model_id: None,
                    model_fingerprint: None,
                    reasoning_effort: None,
                }));
            }
            LlmMessage::Tool {
                tool_call_id,
                content,
                ..
            } => {
                items.push(ConversationItem::ToolResult(ToolResultItem {
                    tool_call_id: tool_call_id.clone(),
                    content: Arc::<str>::from(content.as_str()),
                    images: Vec::new(),
                }));
            }
        }
    }

    let kernel_tools: Vec<ToolSpec> = tools
        .iter()
        .map(|t| ToolSpec {
            name: t.name.clone(),
            description: Some(t.description.clone()),
            parameters: t.parameters.clone(),
        })
        .collect();

    ConversationRequest {
        items,
        tools: kernel_tools,
        ..Default::default()
    }
}

/// Stateful zero-latency `<think>...</think>` stream filter so reasoning models
/// (e.g. MiniMax-M3.1-Flash, MiniMax-M2.7, DeepSeek-R1, QwQ) never leak raw `<think>` blocks into chat bubbles.
#[derive(Default)]
struct ThinkTagStreamFilter {
    in_think: bool,
    emitted_any: bool,
    pending: String,
}

impl ThinkTagStreamFilter {
    fn push(&mut self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }
        self.pending.push_str(chunk);
        let mut out = String::new();

        loop {
            if self.in_think {
                if let Some(end_idx) = self.pending.find("</think>") {
                    let rest = self.pending[end_idx + "</think>".len()..].to_string();
                    self.pending = if self.emitted_any {
                        rest
                    } else {
                        rest.trim_start_matches(['\r', '\n']).to_string()
                    };
                    self.in_think = false;
                    continue;
                }
                const CLOSE_PREFIXES: [&str; 7] = [
                    "</think", "</thin", "</thi", "</th", "</t", "</", "<",
                ];
                let keep = CLOSE_PREFIXES
                    .iter()
                    .find(|p| self.pending.ends_with(**p))
                    .copied()
                    .unwrap_or("");
                self.pending.clear();
                self.pending.push_str(keep);
                break;
            } else {
                if let Some(start_idx) = self.pending.find("<think>") {
                    let before = &self.pending[..start_idx];
                    if !before.is_empty() {
                        out.push_str(before);
                        self.emitted_any = true;
                    }
                    self.pending = self.pending[start_idx + "<think>".len()..].to_string();
                    self.in_think = true;
                    continue;
                }
                const OPEN_PREFIXES: [&str; 6] = ["<think", "<thin", "<thi", "<th", "<t", "<"];
                let keep_len = OPEN_PREFIXES
                    .iter()
                    .find(|p| self.pending.ends_with(**p))
                    .map(|p| p.len())
                    .unwrap_or(0);
                let emit_end = self.pending.len() - keep_len;
                if emit_end > 0 {
                    let emit_part = &self.pending[..emit_end];
                    if !self.emitted_any {
                        let trimmed = emit_part.trim_start_matches(['\r', '\n']);
                        if !trimmed.is_empty() {
                            out.push_str(trimmed);
                            self.emitted_any = true;
                        }
                    } else {
                        out.push_str(emit_part);
                    }
                    self.pending = self.pending[emit_end..].to_string();
                }
                break;
            }
        }
        out
    }

    fn finish(&mut self) -> String {
        if self.in_think {
            self.pending.clear();
            String::new()
        } else {
            std::mem::take(&mut self.pending)
        }
    }
}

fn strip_think_blocks(raw: &str) -> String {
    let mut filter = ThinkTagStreamFilter::default();
    let mut out = filter.push(raw);
    out.push_str(&filter.finish());
    out.trim().to_string()
}

/// Streams a completion via `agent-kernel`'s [`MultiProviderRouter`] (`SamplerActor` + `Eventsource` SSE),
/// invoking `on_text_delta` live as each `SamplingEvent::ChannelToken` arrives over the wire.
pub async fn stream_with_failover<F>(
    state: &AppState,
    space_id: i64,
    role: ModelRole,
    system_static: &str,
    ltm_block: &str,
    messages: &[LlmMessage],
    tools: &[ToolSpecWire],
    mut on_text_delta: F,
) -> Result<LlmCompletion, String>
where
    F: FnMut(&str) + Send,
{
    let mut providers = load_effective_providers(state, space_id).await;
    if providers.is_empty() {
        return Ok(builtin_companion_fallback(role, messages, tools));
    }

    providers.sort_by_key(|p| {
        let is_prim = match role {
            ModelRole::Chat => p.is_chat_primary,
            ModelRole::Memory => p.is_memory_primary,
        };
        (if is_prim { 0 } else { 1 }, p.priority)
    });

    let endpoints: Vec<ProviderEndpoint> = providers
        .iter()
        .flat_map(|p| expand_kernel_endpoints(p, role))
        .collect();

    if endpoints.is_empty() {
        return Ok(builtin_companion_fallback(role, messages, tools));
    }

    let router = MultiProviderRouter::new(endpoints);
    let kernel_req = build_kernel_conversation_request(system_static, ltm_block, messages, tools);

    let primary_id_for_health = providers.first().map(|p| p.id.clone());
    let mut think_filter = ThinkTagStreamFilter::default();
    let sample_res = router
        .sample_streaming(role.into(), kernel_req, |ev| {
            if let SamplingEvent::ChannelToken {
                channel: SamplingChannel::Text,
                text,
                ..
            } = ev
            {
                if !text.is_empty() {
                    let clean = think_filter.push(text);
                    if !clean.is_empty() {
                        on_text_delta(&clean);
                    }
                }
            }
        })
        .await;

    let tail = think_filter.finish();
    if !tail.is_empty() {
        on_text_delta(&tail);
    }

    let (response, _latency_stats, provider_id) = match sample_res {
        Ok(ok) => ok,
        Err(e) => {
            let err_str = e.to_string();
            if let Some(pid) = &primary_id_for_health {
                set_cached_provider_health(state, pid, "error", Some(&err_str)).await;
            }
            return Err(err_str);
        }
    };
    set_cached_provider_health(state, &provider_id, "connected", None).await;

    let text = response
        .assistant()
        .map(|a| strip_think_blocks(&a.content))
        .unwrap_or_default();

    let tool_calls = response
        .tool_calls()
        .into_iter()
        .map(|tc| {
            let args_val = serde_json::from_str::<Value>(&tc.arguments)
                .unwrap_or_else(|_| json!({}));
            ToolCallItem {
                id: tc.id.to_string(),
                name: tc.name.clone(),
                arguments: args_val,
            }
        })
        .collect();

    Ok(LlmCompletion {
        text,
        tool_calls,
        provider_used: provider_id,
    })
}

/// Non-streaming convenience wrapper around [`stream_with_failover`] for background memory & compaction tasks.
pub async fn complete_with_failover(
    state: &AppState,
    space_id: i64,
    role: ModelRole,
    system_static: &str,
    ltm_block: &str,
    messages: &[LlmMessage],
    tools: &[ToolSpecWire],
) -> Result<LlmCompletion, String> {
    stream_with_failover(
        state,
        space_id,
        role,
        system_static,
        ltm_block,
        messages,
        tools,
        |_| {},
    )
    .await
}

/// Out-of-the-box local companion responder when zero external API keys are configured yet.
/// Capable of driving KeepDays tools and emitting real Google A2UI dynamic surfaces!
fn builtin_companion_fallback(
    role: ModelRole,
    messages: &[LlmMessage],
    tools: &[ToolSpecWire],
) -> LlmCompletion {
    if role == ModelRole::Memory {
        let joined: String = messages
            .iter()
            .filter_map(|m| match m {
                LlmMessage::User { content } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let fallback_json = json!({
            "private_summary": joined.chars().take(180).collect::<String>(),
            "shared_summary": "",
            "clarifications": [],
            "ltm_ops": []
        });
        return LlmCompletion {
            text: fallback_json.to_string(),
            tool_calls: Vec::new(),
            provider_used: "builtin:local".to_string(),
        };
    }

    if let Some(LlmMessage::Tool { name, content, .. }) = messages.last() {
        if name == "a2ui_surface" {
            return LlmCompletion {
                text: "我把小卡片摆在上面啦，你直接点选或勾选就行，选完我马上帮你办妥～".to_string(),
                tool_calls: Vec::new(),
                provider_used: "builtin:local".to_string(),
            };
        }
        return LlmCompletion {
            text: format!(
                "已经帮你查看好啦：{}\n*(提示：在左下角「工具箱 -> 模型」选择厂商并填入密钥即可解锁完整推理)*",
                content.chars().take(220).collect::<String>()
            ),
            tool_calls: Vec::new(),
            provider_used: "builtin:local".to_string(),
        };
    }

    let last_user = messages
        .iter()
        .rev()
        .find_map(|m| match m {
            LlmMessage::User { content } => Some(content.as_str()),
            _ => None,
        })
        .unwrap_or("");

    if last_user.starts_with("[A2UI 交互回传]") && last_user.contains("confirm_dinner_menu") {
        if let Some(dm_pos) = last_user.find("data_model=") {
            let dm_slice = last_user[dm_pos + "data_model=".len()..]
                .lines()
                .next()
                .unwrap_or("{}");
            if let Ok(dm) = serde_json::from_str::<Value>(dm_slice) {
                let dishes = dm
                    .get("dishes")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if !dishes.is_empty() {
                    let less_spicy = dm
                        .get("less_spicy")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let raw_note = dm.get("note").and_then(|v| v.as_str()).unwrap_or("").trim();
                    let note = match (less_spicy, raw_note.is_empty()) {
                        (true, true) => "少辣清淡".to_string(),
                        (true, false) => format!("少辣 · {raw_note}"),
                        (false, _) => raw_note.to_string(),
                    };
                    return LlmCompletion {
                        text: String::new(),
                        tool_calls: vec![ToolCallItem {
                            id: format!("call_pin_{}", now_ms()),
                            name: "use_tool".to_string(),
                            arguments: json!({
                                "tool_name": "fridge__pin_today_menu",
                                "arguments": {
                                    "dishes": dishes,
                                    "note": note,
                                }
                            }),
                        }],
                        provider_used: "builtin:local".to_string(),
                    };
                }
            }
        }
    }

    let has_a2ui = tools.iter().any(|t| t.name == "a2ui_surface");
    if has_a2ui
        && (last_user.contains("吃什么")
            || last_user.contains("菜单")
            || last_user.contains("点菜"))
    {
        return LlmCompletion {
            text: String::new(),
            tool_calls: vec![ToolCallItem {
                id: format!("call_a2ui_{}", now_ms()),
                name: "a2ui_surface".to_string(),
                arguments: json!({
                    "surface": {
                        "surface_id": format!("dinner_{}", now_ms()),
                        "title": "今晚想吃哪几道？",
                        "root_id": "card_root",
                        "data_model": {
                            "dishes": [],
                            "less_spicy": false,
                            "note": ""
                        },
                        "components": [
                            { "id": "card_root", "component": "Card", "children": ["hint", "dish_list", "spicy_row", "note_in"] },
                            { "id": "hint", "component": "Text", "props": { "text": "直接打勾，停顿一下就会自动发给我贴上冰箱门：" } },
                            {
                                "id": "dish_list",
                                "component": "CheckboxGroup",
                                "bind_path": "/dishes",
                                "props": {
                                    "options": [
                                        { "value": "可乐鸡翅", "label": "可乐鸡翅 · 荤菜" },
                                        { "value": "番茄滑蛋", "label": "番茄滑蛋 · 家常" },
                                        { "value": "清炒时蔬", "label": "清炒时蔬 · 素菜" },
                                        { "value": "冬瓜排骨汤", "label": "冬瓜排骨汤 · 汤羹" }
                                    ]
                                }
                            },
                            {
                                "id": "spicy_row",
                                "component": "Switch",
                                "bind_path": "/less_spicy",
                                "props": { "label": "少放辣椒 / 清淡一点" }
                            },
                            {
                                "id": "note_in",
                                "component": "TextField",
                                "bind_path": "/note",
                                "props": { "placeholder": "顺便留句备注（如：七点半开饭）" }
                            }
                        ],
                        "trigger": { "mode": "debounce", "delay_ms": 1400, "action": "confirm_dinner_menu" }
                    }
                }),
            }],
            provider_used: "builtin:local".to_string(),
        };
    }

    if has_a2ui && (last_user.contains("图") || last_user.contains("统计") || last_user.contains("数据")) {
        return LlmCompletion {
            text: String::new(),
            tool_calls: vec![ToolCallItem {
                id: format!("call_chart_{}", now_ms()),
                name: "a2ui_surface".to_string(),
                arguments: json!({
                    "surface": {
                        "surface_id": format!("stats_{}", now_ms()),
                        "title": "小家近期互动一览",
                        "root_id": "root",
                        "components": [
                            { "id": "root", "component": "Card", "children": ["m_row", "c_bar", "yn_btns"] },
                            { "id": "m_row", "component": "Row", "children": ["m1", "m2"] },
                            { "id": "m1", "component": "Metric", "props": { "label": "本周默契度", "value": "98%", "delta": "+12%" } },
                            { "id": "m2", "component": "Metric", "props": { "label": "定格瞬间", "value": "24 张", "delta": "持续记录中" } },
                            {
                                "id": "c_bar",
                                "component": "Chart",
                                "props": {
                                    "kind": "bar",
                                    "data": [
                                        { "label": "荤菜", "value": 8 },
                                        { "label": "素菜", "value": 6 },
                                        { "label": "汤羹", "value": 4 },
                                        { "label": "甜饮", "value": 5 }
                                    ]
                                }
                            },
                            {
                                "id": "yn_btns",
                                "component": "ActionGroup",
                                "props": {
                                    "options": [
                                        { "label": "按这个偏好推荐今晚菜单", "value": "recommend_now" },
                                        { "label": "挺好，继续保持", "value": "keep_going" }
                                    ]
                                }
                            }
                        ],
                        "trigger": { "mode": "immediate", "action": "stats_followup" }
                    }
                }),
            }],
            provider_used: "builtin:local".to_string(),
        };
    }

    LlmCompletion {
        text: "喵呜～我趴在冰箱顶上听着呢！你可以让我帮你看冰箱便签、挑今晚吃什么（我会给你弹勾选单）、画小家统计图，或者点右上角齿轮配置你的大模型供应商密钥。".to_string(),
        tool_calls: Vec::new(),
        provider_used: "builtin:local".to_string(),
    }
}
