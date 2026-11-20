//! Native Google Gemini `/models/{model}:streamGenerateContent?alt=sse` adapter.
//!
//! Converts [`ConversationRequest`] into Gemini's native `generateContent` JSON wire format
//! (`systemInstruction`, `contents`, `tools: [{ functionDeclarations }]`, `generationConfig`)
//! and translates streaming `GenerateContentResponse` SSE chunks into [`ChatCompletionChunk`]
//! so Layer-2 [`crate::stream::stream_chat_completions`] and Layer-3 [`crate::SamplerActor`]
//! drive Gemini streams natively with full retry, cancellation, reasoning, and tool-call support.

use serde_json::{Value, json};
use xai_grok_sampling_types::{
    ChatChunkChoice, ChatChunkDelta, ChatCompletionChunk, ChatCompletionRequest, ChatContentBlock,
    CompletionTokensDetails, ConversationRequest, FinishReason, MessageContent, Result, Role,
    SamplingError, ToolCallDelta, ToolCallFunctionDelta, Usage,
};

/// Build the relative Gemini streaming path for `model`, e.g.
/// `models/gemini-2.5-pro:streamGenerateContent`.
pub fn gemini_stream_path(model: &str) -> String {
    let trimmed = model.trim().trim_start_matches("models/");
    format!("models/{trimmed}:streamGenerateContent")
}

/// Sanitize a JSON Schema recursively for Gemini `functionDeclarations.parameters`,
/// stripping unsupported meta-keywords (`$schema`, `additionalProperties`) that
/// Gemini's strict OpenAPI-subset validator rejects with HTTP 400.
fn sanitize_gemini_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if matches!(
                    k.as_str(),
                    "$schema" | "additionalProperties" | "strict" | "$defs" | "definitions"
                ) {
                    continue;
                }
                out.insert(k.clone(), sanitize_gemini_schema(v));
            }
            if !out.contains_key("type") && out.contains_key("properties") {
                out.insert("type".to_string(), Value::String("object".to_string()));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sanitize_gemini_schema).collect()),
        other => other.clone(),
    }
}

/// Translate a [`ConversationRequest`] (via [`ChatCompletionRequest`]) into a Gemini
/// `generateContent` JSON request body plus resolved model id.
pub fn build_gemini_request_body(
    request: ConversationRequest,
    default_model: &str,
    default_max_tokens: Option<u32>,
    default_temperature: Option<f32>,
    default_top_p: Option<f32>,
) -> (String, Value) {
    let chat_req = ChatCompletionRequest::from(request);
    let model = chat_req
        .model
        .clone()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| default_model.to_string());

    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    let mut call_id_to_name = std::collections::HashMap::<String, String>::new();

    for msg in &chat_req.messages {
        match msg.role {
            Role::System => {
                let text = msg.text_content();
                if !text.trim().is_empty() {
                    system_parts.push(json!({ "text": text }));
                }
            }
            Role::User => {
                let mut parts = Vec::new();
                match &msg.content {
                    MessageContent::Text(s) => {
                        parts.push(json!({ "text": s }));
                    }
                    MessageContent::Blocks(blocks) => {
                        for block in blocks {
                            match block {
                                ChatContentBlock::Text { text } => {
                                    parts.push(json!({ "text": text }));
                                }
                                ChatContentBlock::ImageUrl { image_url } => {
                                    if let Some(rest) = image_url.url.strip_prefix("data:")
                                        && let Some((mime, b64)) = rest.split_once(";base64,")
                                    {
                                        parts.push(json!({
                                            "inlineData": {
                                                "mimeType": mime,
                                                "data": b64,
                                            }
                                        }));
                                    } else {
                                        parts.push(json!({
                                            "fileData": {
                                                "fileUri": image_url.url,
                                            }
                                        }));
                                    }
                                }
                            }
                        }
                    }
                }
                if !parts.is_empty() {
                    contents.push(json!({
                        "role": "user",
                        "parts": parts,
                    }));
                }
            }
            Role::Assistant => {
                let mut parts = Vec::new();
                let text = msg.text_content();
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
                for tc in &msg.tool_calls {
                    if let Some(id) = &tc.id {
                        call_id_to_name.insert(id.clone(), tc.function.name.clone());
                    }
                    let args_val = serde_json::from_str::<Value>(&tc.function.arguments)
                        .unwrap_or_else(|_| json!({}));
                    parts.push(json!({
                        "functionCall": {
                            "name": tc.function.name,
                            "args": args_val,
                        }
                    }));
                }
                if !parts.is_empty() {
                    contents.push(json!({
                        "role": "model",
                        "parts": parts,
                    }));
                }
            }
            Role::Tool => {
                let fn_name = msg
                    .tool_call_id
                    .as_deref()
                    .and_then(|id| call_id_to_name.get(id).cloned())
                    .or_else(|| msg.name.clone())
                    .unwrap_or_else(|| "tool_result".to_string());
                let text = msg.text_content();
                let response_obj = match serde_json::from_str::<Value>(&text) {
                    Ok(Value::Object(map)) => Value::Object(map),
                    Ok(other) => json!({ "output": other }),
                    Err(_) => json!({ "output": text }),
                };
                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": fn_name,
                            "response": response_obj,
                        }
                    }]
                }));
            }
        }
    }

    if contents.is_empty() {
        contents.push(json!({
            "role": "user",
            "parts": [{ "text": "" }],
        }));
    }

    let mut body = serde_json::Map::new();
    if !system_parts.is_empty() {
        body.insert(
            "systemInstruction".to_string(),
            json!({ "parts": system_parts }),
        );
    }
    body.insert("contents".to_string(), Value::Array(contents));

    if let Some(tools) = &chat_req.tools
        && !tools.is_empty()
    {
        let mut decls = Vec::with_capacity(tools.len());
        for t in tools {
            let mut decl = serde_json::Map::new();
            decl.insert("name".to_string(), Value::String(t.function.name.clone()));
            if let Some(desc) = &t.function.description {
                decl.insert("description".to_string(), Value::String(desc.clone()));
            }
            if !t.function.parameters.is_null() {
                decl.insert(
                    "parameters".to_string(),
                    sanitize_gemini_schema(&t.function.parameters),
                );
            }
            decls.push(Value::Object(decl));
        }
        body.insert(
            "tools".to_string(),
            json!([{ "functionDeclarations": decls }]),
        );
    }

    let mut gen_cfg = serde_json::Map::new();
    if let Some(max_tokens) = chat_req.max_tokens.or(default_max_tokens) {
        gen_cfg.insert("maxOutputTokens".to_string(), json!(max_tokens));
    }
    if let Some(temp) = chat_req.temperature.or(default_temperature) {
        gen_cfg.insert("temperature".to_string(), json!(temp));
    }
    if let Some(top_p) = chat_req.top_p.or(default_top_p) {
        gen_cfg.insert("topP".to_string(), json!(top_p));
    }
    if !gen_cfg.is_empty() {
        body.insert("generationConfig".to_string(), Value::Object(gen_cfg));
    }

    (model, Value::Object(body))
}

/// Parse one Gemini SSE `data:` JSON payload into a [`ChatCompletionChunk`].
pub fn parse_gemini_sse_chunk(
    data: &str,
    model_id: &str,
    next_tool_idx: &mut u32,
) -> Result<ChatCompletionChunk> {
    let v: Value = serde_json::from_str(data).map_err(SamplingError::Serialization)?;
    if let Some(err_obj) = v.get("error") {
        let msg = err_obj
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Gemini stream error")
            .to_string();
        return Err(SamplingError::EventStreamError(msg));
    }

    let mut content_text = String::new();
    let mut reasoning_text = String::new();
    let mut tool_calls: Vec<ToolCallDelta> = Vec::new();
    let mut finish_reason: Option<FinishReason> = None;

    if let Some(first_cand) = v
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
    {
        if let Some(parts) = first_cand
            .pointer("/content/parts")
            .and_then(|p| p.as_array())
        {
            for part in parts {
                let is_thought = part
                    .get("thought")
                    .and_then(|t| t.as_bool())
                    .unwrap_or(false);
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    if is_thought {
                        reasoning_text.push_str(text);
                    } else {
                        content_text.push_str(text);
                    }
                }
                if let Some(fc) = part.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let args_str = fc
                        .get("args")
                        .map(|a| serde_json::to_string(a).unwrap_or_else(|_| "{}".to_string()))
                        .unwrap_or_else(|| "{}".to_string());
                    let idx = *next_tool_idx;
                    *next_tool_idx = next_tool_idx.saturating_add(1);
                    let call_id = format!("gemini_call_{}_{}", idx, uuid::Uuid::new_v4().simple());
                    tool_calls.push(ToolCallDelta {
                        index: idx,
                        id: Some(call_id),
                        kind: Some("function".to_string()),
                        function: Some(ToolCallFunctionDelta {
                            name: Some(name),
                            arguments: Some(args_str),
                        }),
                    });
                }
            }
        }

        if let Some(fr) = first_cand.get("finishReason").and_then(|f| f.as_str()) {
            finish_reason = match fr {
                "MAX_TOKENS" => Some(FinishReason::Length),
                "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" => {
                    Some(FinishReason::ContentFilter)
                }
                "STOP" => {
                    if !tool_calls.is_empty() {
                        Some(FinishReason::ToolCalls)
                    } else {
                        Some(FinishReason::Stop)
                    }
                }
                _ => None,
            };
        }
    }

    let usage = v.get("usageMetadata").map(|u| {
        let prompt_tokens = u
            .get("promptTokenCount")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32;
        let completion_tokens = u
            .get("candidatesTokenCount")
            .and_then(|n| n.as_u64())
            .unwrap_or(0) as u32;
        let thoughts_tokens = u
            .get("thoughtsTokenCount")
            .and_then(|n| n.as_u64())
            .map(|n| n as u32);
        let total_tokens = u
            .get("totalTokenCount")
            .and_then(|n| n.as_u64())
            .map(|n| n as u32)
            .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
        Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            prompt_tokens_details: None,
            completion_tokens_details: thoughts_tokens.map(|r| CompletionTokensDetails {
                reasoning_tokens: r,
                ..Default::default()
            }),
            cost_in_usd_ticks: None,
        }
    });

    let choice = ChatChunkChoice {
        index: 0,
        delta: ChatChunkDelta {
            role: Some(Role::Assistant),
            content: (!content_text.is_empty()).then_some(content_text),
            reasoning_content: (!reasoning_text.is_empty()).then_some(reasoning_text),
            tool_calls,
            tool_call_id: None,
        },
        finish_reason,
    };

    let response_model = v
        .get("modelVersion")
        .and_then(|m| m.as_str())
        .unwrap_or(model_id)
        .to_string();

    Ok(ChatCompletionChunk {
        id: v
            .get("responseId")
            .and_then(|id| id.as_str())
            .unwrap_or("gemini-stream")
            .to_string(),
        object: "chat.completion.chunk".to_string(),
        created: 0,
        model: response_model,
        system_fingerprint: None,
        choices: vec![choice],
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_sampling_types::{ConversationItem, ToolSpec};

    #[test]
    fn builds_gemini_request_and_strips_unsupported_schema_fields() {
        let mut req = ConversationRequest {
            items: vec![
                ConversationItem::system("你是糯米，一只温暖的伴侣小宠。"),
                ConversationItem::user("今晚吃什么？"),
            ],
            model: Some("gemini-2.5-flash".to_string()),
            tools: vec![ToolSpec {
                name: "a2ui_surface".to_string(),
                description: Some("Render dynamic UI".to_string()),
                parameters: json!({
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "title": { "type": "string" }
                    }
                }),
            }],
            ..Default::default()
        };
        req.temperature = Some(0.7);
        let (model, body) = build_gemini_request_body(req, "gemini-2.5-pro", Some(4096), None, None);
        assert_eq!(model, "gemini-2.5-flash");
        assert_eq!(
            body.pointer("/systemInstruction/parts/0/text")
                .and_then(|v| v.as_str()),
            Some("你是糯米，一只温暖的伴侣小宠。")
        );
        let params = body
            .pointer("/tools/0/functionDeclarations/0/parameters")
            .expect("parameters");
        assert!(params.get("$schema").is_none());
        assert!(params.get("additionalProperties").is_none());
        assert_eq!(
            params.pointer("/properties/title/type").and_then(|v| v.as_str()),
            Some("string")
        );
    }

    #[test]
    fn parses_gemini_text_thought_and_function_call_chunks() {
        let mut tool_idx = 0;
        let raw = r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {"text": "先看看冰箱里有什么", "thought": true},
                        {"text": "今晚做番茄牛腩怎么样？"},
                        {"functionCall": {"name": "fridge__list_items", "args": {"category": "fresh"}}}
                    ]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 42,
                "candidatesTokenCount": 18,
                "thoughtsTokenCount": 9,
                "totalTokenCount": 69
            },
            "modelVersion": "gemini-2.5-flash"
        }"#;
        let chunk = parse_gemini_sse_chunk(raw, "gemini-2.5-flash", &mut tool_idx).unwrap();
        let choice = &chunk.choices[0];
        assert_eq!(
            choice.delta.reasoning_content.as_deref(),
            Some("先看看冰箱里有什么")
        );
        assert_eq!(
            choice.delta.content.as_deref(),
            Some("今晚做番茄牛腩怎么样？")
        );
        let tc = &choice.delta.tool_calls[0];
        assert_eq!(
            tc.function.as_ref().unwrap().name.as_deref(),
            Some("fridge__list_items")
        );
        assert_eq!(choice.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(chunk.usage.unwrap().total_tokens, 69);
    }
}
