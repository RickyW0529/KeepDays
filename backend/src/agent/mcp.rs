use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex as StdMutex, RwLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::{state::AppState, time_bj::now_ms};

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const MCP_CONNECTED_CACHE_TTL_MS: i64 = 300_000; // 5 minutes
const MCP_ERROR_CACHE_TTL_MS: i64 = 20_000; // 20 seconds

static MCP_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent("keepdays-mcp-client/1.0")
        .timeout(Duration::from_secs(22))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
});

#[derive(Debug, Clone)]
struct CachedMcpEntry {
    fingerprint: String,
    session_id: Option<String>,
    resolved_transport: String, // "http" | "sse" | "stdio"
    server_instructions: String,
    tools: Vec<McpDiscoveredTool>,
    health_status: String, // "connected" | "auth_required" | "error"
    health_error: Option<String>,
    updated_at_ms: i64,
}

static MCP_RUNTIME_CACHE: LazyLock<RwLock<HashMap<String, CachedMcpEntry>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

static MCP_IN_FLIGHT_PROBES: LazyLock<StdMutex<HashSet<String>>> =
    LazyLock::new(|| StdMutex::new(HashSet::new()));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfigDto {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `"http"` | `"sse"` | `"stdio"`
    #[serde(default = "default_mcp_transport")]
    pub transport: String,
    pub endpoint: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default = "default_mcp_enabled")]
    pub enabled: bool,
    /// Populated live on read/probe: `"connected"` | `"auth_required"` | `"error"` | `"disabled"`
    #[serde(default)]
    pub health_status: String,
    #[serde(default)]
    pub health_error: Option<String>,
    #[serde(default)]
    pub tool_count: usize,
    #[serde(default)]
    pub tool_names: Vec<String>,
}

fn default_mcp_transport() -> String {
    "http".to_string()
}

fn default_mcp_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpDiscoveredTool {
    pub qualified_name: String, // e.g. `mcp__weather__openmeteo_get_forecast`
    pub server_id: String,
    pub server_name: String,
    pub remote_tool_name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct MountedMcpOverview {
    pub server_name: String,
    pub instructions: String,
    pub tools: Vec<McpDiscoveredTool>,
}

fn server_fingerprint(srv: &McpServerConfigDto) -> String {
    let mut hdr_pairs: Vec<(&String, &String)> = srv.headers.iter().collect();
    hdr_pairs.sort_by_key(|(k, _)| *k);
    format!(
        "{}|{}|{}|{:?}|{:?}|{}",
        srv.name, srv.transport, srv.endpoint, srv.args, hdr_pairs, srv.enabled
    )
}

pub fn invalidate_mcp_cache(space_id: i64, mcp_id: Option<&str>) {
    if let Ok(mut guard) = MCP_RUNTIME_CACHE.write() {
        match mcp_id {
            Some(id) => {
                let key = format!("{space_id}:{id}");
                guard.remove(&key);
            }
            None => {
                let prefix = format!("{space_id}:");
                guard.retain(|k, _| !k.starts_with(&prefix));
            }
        }
    }
}

fn sanitize_server_slug(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let trimmed = s.trim_matches('_');
    if trimmed.is_empty() {
        "ext".to_string()
    } else {
        trimmed.chars().take(20).collect()
    }
}

fn effective_transport_for(srv: &McpServerConfigDto) -> &'static str {
    let ep = srv.endpoint.trim();
    let t = srv.transport.trim().to_ascii_lowercase();
    if t == "stdio" || (!ep.starts_with("http://") && !ep.starts_with("https://")) {
        "stdio"
    } else if t == "sse" || ep.trim_end_matches('/').ends_with("/sse") {
        "sse"
    } else {
        "http"
    }
}

async fn query_raw_mcp_servers(
    state: &AppState,
    space_id: i64,
    only_enabled: bool,
) -> Vec<McpServerConfigDto> {
    let mut list = Vec::new();
    let sql = if only_enabled {
        "SELECT id, name, description, transport, endpoint, args_json, headers_json, enabled
         FROM agent_mcp_servers
         WHERE space_id IN (?1, 0) AND enabled = 1
         ORDER BY updated_at DESC"
    } else {
        "SELECT id, name, description, transport, endpoint, args_json, headers_json, enabled
         FROM agent_mcp_servers
         WHERE space_id IN (?1, 0)
         ORDER BY updated_at DESC"
    };

    if let Ok(rows) = sqlx::query(sql)
        .bind(space_id)
        .fetch_all(&state.db.read)
        .await
    {
        for r in rows {
            let args: Vec<String> = serde_json::from_str(r.get("args_json")).unwrap_or_default();
            let headers: HashMap<String, String> =
                serde_json::from_str(r.get("headers_json")).unwrap_or_default();
            let enabled = r.get::<i64, _>("enabled") != 0;
            list.push(McpServerConfigDto {
                id: r.get("id"),
                name: r.get("name"),
                description: r.get("description"),
                transport: r.get("transport"),
                endpoint: r.get("endpoint"),
                args,
                headers,
                enabled,
                health_status: if enabled {
                    "unknown".to_string()
                } else {
                    "disabled".to_string()
                },
                health_error: None,
                tool_count: 0,
                tool_names: Vec::new(),
            });
        }
    }
    list
}

/// Loads all configured MCP servers for UI display, enriching each enabled server with live/cached
/// connectivity status (`connected` / `auth_required` / `error`) and discovered tool counts.
pub async fn load_mcp_servers(state: &AppState, space_id: i64) -> Vec<McpServerConfigDto> {
    let mut servers = query_raw_mcp_servers(state, space_id, false).await;
    for srv in &mut servers {
        if !srv.enabled {
            srv.health_status = "disabled".to_string();
            continue;
        }
        let entry = ensure_server_probed(space_id, srv, false).await;
        srv.health_status = entry.health_status;
        srv.health_error = entry.health_error;
        srv.tool_count = entry.tools.len();
        srv.tool_names = entry
            .tools
            .iter()
            .map(|t| t.remote_tool_name.clone())
            .collect();
    }
    servers
}

/// Force-probes a single MCP server (`force = true`) and refreshes its cached tool catalog & health status.
pub async fn probe_single_mcp_server(state: &AppState, space_id: i64, mcp_id: &str) {
    invalidate_mcp_cache(space_id, Some(mcp_id));
    let servers = query_raw_mcp_servers(state, space_id, false).await;
    if let Some(srv) = servers.into_iter().find(|s| s.id == mcp_id) {
        if srv.enabled {
            let _ = ensure_server_probed(space_id, &srv, true).await;
        }
    }
}

/// Returns all discovered MCP tools across enabled servers for `search_tool` & `use_tool`.
pub async fn discover_mcp_tools(state: &AppState, space_id: i64) -> Vec<McpDiscoveredTool> {
    let servers = query_raw_mcp_servers(state, space_id, true).await;
    let mut out = Vec::new();
    for srv in servers {
        let entry = ensure_server_probed(space_id, &srv, false).await;
        out.extend(entry.tools);
    }
    out
}

/// Returns a structured overview of connected MCP servers & their workflow instructions for the system prompt.
/// Non-blocking on stale/failing servers: serves cached entries immediately and refreshes expired servers in background
/// (waiting at most 900ms on a completely cold first probe so chat TTFB never stalls 20+ seconds!).
pub async fn discover_mounted_mcp_overviews(
    state: &AppState,
    space_id: i64,
) -> Vec<MountedMcpOverview> {
    let servers = query_raw_mcp_servers(state, space_id, true).await;
    let mut out = Vec::new();
    let now = now_ms();

    for srv in servers {
        let cache_key = format!("{space_id}:{}", srv.id);
        let fp = server_fingerprint(&srv);
        let cached = MCP_RUNTIME_CACHE
            .read()
            .ok()
            .and_then(|g| g.get(&cache_key).cloned())
            .filter(|e| e.fingerprint == fp);

        let entry = match cached {
            Some(existing) => {
                let ttl = if existing.health_status == "connected" {
                    MCP_CONNECTED_CACHE_TTL_MS
                } else {
                    MCP_ERROR_CACHE_TTL_MS
                };
                if now - existing.updated_at_ms >= ttl {
                    spawn_background_mcp_probe(space_id, srv.clone());
                }
                existing
            }
            None => {
                // First cold access: give fast local/HTTP servers up to 900ms, otherwise continue in background!
                let srv_bg = srv.clone();
                let handle = tokio::spawn(async move {
                    ensure_server_probed(space_id, &srv_bg, false).await
                });
                match tokio::time::timeout(Duration::from_millis(900), handle).await {
                    Ok(Ok(probed)) => probed,
                    _ => continue,
                }
            }
        };

        if entry.health_status == "connected" && !entry.tools.is_empty() {
            out.push(MountedMcpOverview {
                server_name: srv.name.clone(),
                instructions: entry.server_instructions,
                tools: entry.tools,
            });
        }
    }
    out
}

fn spawn_background_mcp_probe(space_id: i64, srv: McpServerConfigDto) {
    let cache_key = format!("{space_id}:{}", srv.id);
    if let Ok(mut set) = MCP_IN_FLIGHT_PROBES.lock() {
        if !set.insert(cache_key.clone()) {
            return;
        }
    }
    tokio::spawn(async move {
        let _ = ensure_server_probed(space_id, &srv, true).await;
        if let Ok(mut set) = MCP_IN_FLIGHT_PROBES.lock() {
            set.remove(&cache_key);
        }
    });
}

/// Invokes an MCP tool by either its qualified name (`mcp__weather__openmeteo_get_forecast`)
/// OR its bare remote tool name (`openmeteo_get_forecast`), returning formatted text & `is_error`.
pub async fn invoke_mcp_tool(
    state: &AppState,
    space_id: i64,
    requested_name: &str,
    arguments: Value,
) -> Result<(String, String), String> {
    let trimmed = requested_name.trim();
    let tools = discover_mcp_tools(state, space_id).await;
    let hit = tools
        .into_iter()
        .find(|t| {
            t.qualified_name == trimmed
                || t.remote_tool_name == trimmed
                || trimmed.ends_with(&format!("__{}", t.remote_tool_name))
        })
        .ok_or_else(|| format!("未找到 MCP 工具 `{trimmed}`"))?;

    let servers = query_raw_mcp_servers(state, space_id, true).await;
    let srv = servers
        .into_iter()
        .find(|s| s.id == hit.server_id)
        .ok_or_else(|| format!("MCP 服务 `{}` 已停用或移除", hit.server_name))?;

    let cache_key = format!("{space_id}:{}", srv.id);
    let cached_entry = MCP_RUNTIME_CACHE
        .read()
        .ok()
        .and_then(|g| g.get(&cache_key).cloned());

    let call_params = json!({
        "name": hit.remote_tool_name,
        "arguments": arguments,
    });

    let res = match cached_entry
        .as_ref()
        .map(|e| e.resolved_transport.as_str())
        .unwrap_or_else(|| effective_transport_for(&srv))
    {
        "stdio" => mcp_stdio_call(&srv, "tools/call", call_params).await?.1,
        "sse" => mcp_legacy_sse_call(&srv, "tools/call", call_params).await?.1,
        _ => {
            let sid = cached_entry.and_then(|e| e.session_id);
            match mcp_streamable_http_call(&srv, sid.as_deref(), "tools/call", call_params.clone())
                .await
            {
                Ok(v) => v,
                Err(e) if e.contains("SESSION_EXPIRED") || e.contains("HTTP 404") || e.contains("HTTP 400") => {
                    // Session expired on stateful Streamable HTTP server -> re-initialize once transparently!
                    let fresh = ensure_server_probed(space_id, &srv, true).await;
                    if fresh.health_status != "connected" {
                        return Err(fresh
                            .health_error
                            .unwrap_or_else(|| format!("重连 MCP `{}` 失败", srv.name)));
                    }
                    mcp_streamable_http_call(
                        &srv,
                        fresh.session_id.as_deref(),
                        "tools/call",
                        call_params,
                    )
                    .await?
                }
                Err(e) => return Err(e),
            }
        }
    };

    let is_tool_err = res
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut rendered = String::new();
    if let Some(arr) = res.get("content").and_then(|v| v.as_array()) {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                parts.push(t.to_string());
            } else {
                parts.push(item.to_string());
            }
        }
        if !parts.is_empty() {
            rendered = parts.join("\n");
        }
    }
    if rendered.is_empty() {
        if let Some(sc) = res.get("structuredContent") {
            rendered = serde_json::to_string_pretty(sc).unwrap_or_else(|_| sc.to_string());
        } else {
            rendered = res.to_string();
        }
    }

    if is_tool_err {
        return Err(format!(
            "MCP 工具 `{}` 返回参数/执行提示: {}",
            hit.remote_tool_name, rendered
        ));
    }

    let summary = format!("已调用 {} · {}", hit.server_name, hit.remote_tool_name);
    Ok((summary, rendered))
}

async fn ensure_server_probed(
    space_id: i64,
    srv: &McpServerConfigDto,
    force: bool,
) -> CachedMcpEntry {
    let cache_key = format!("{space_id}:{}", srv.id);
    let fp = server_fingerprint(srv);
    let now = now_ms();

    if !force {
        if let Ok(guard) = MCP_RUNTIME_CACHE.read() {
            if let Some(existing) = guard.get(&cache_key) {
                let ttl = if existing.health_status == "connected" {
                    MCP_CONNECTED_CACHE_TTL_MS
                } else {
                    MCP_ERROR_CACHE_TTL_MS
                };
                if existing.fingerprint == fp && (now - existing.updated_at_ms) < ttl {
                    return existing.clone();
                }
            }
        }
    }

    let probed = probe_mcp_server_uncached(srv).await;
    if let Ok(mut guard) = MCP_RUNTIME_CACHE.write() {
        guard.insert(cache_key, probed.clone());
    }
    probed
}

async fn probe_mcp_server_uncached(srv: &McpServerConfigDto) -> CachedMcpEntry {
    let fp = server_fingerprint(srv);
    let mode = effective_transport_for(srv);

    let outcome: Result<(String, Option<String>, String, Value), String> = match mode {
        "stdio" => mcp_stdio_call(srv, "tools/list", json!({}))
            .await
            .map(|(instr, res)| ("stdio".to_string(), None, instr, res)),
        "sse" => mcp_legacy_sse_call(srv, "tools/list", json!({}))
            .await
            .map(|(instr, res)| ("sse".to_string(), None, instr, res)),
        _ => match mcp_streamable_http_init_and_list(srv).await {
            Ok((sid, instr, res)) => Ok(("http".to_string(), sid, instr, res)),
            Err(http_err) => {
                // If endpoint looks like or behaves like a 2024-11-05 Legacy HTTP+SSE server (404/405 on POST),
                // automatically try Legacy SSE transport before reporting an error!
                if http_err.contains("HTTP 404") || http_err.contains("HTTP 405") {
                    match mcp_legacy_sse_call(srv, "tools/list", json!({})).await {
                        Ok((instr, res)) => Ok(("sse".to_string(), None, instr, res)),
                        Err(_) => Err(http_err),
                    }
                } else {
                    Err(http_err)
                }
            }
        },
    };

    match outcome {
        Ok((resolved_transport, session_id, server_instructions, list_val)) => {
            let safe_srv = sanitize_server_slug(&srv.name);
            let mut tools = Vec::new();
            if let Some(arr) = list_val.get("tools").and_then(|v| v.as_array()) {
                for t in arr {
                    let Some(rname) = t.get("name").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let desc = t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .or_else(|| t.get("title").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .chars()
                        .take(1024)
                        .collect::<String>();
                    let schema = t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                    tools.push(McpDiscoveredTool {
                        qualified_name: format!("mcp__{safe_srv}__{rname}"),
                        server_id: srv.id.clone(),
                        server_name: srv.name.clone(),
                        remote_tool_name: rname.to_string(),
                        description: desc,
                        input_schema: schema,
                    });
                }
            }
            CachedMcpEntry {
                fingerprint: fp,
                session_id,
                resolved_transport,
                server_instructions,
                tools,
                health_status: "connected".to_string(),
                health_error: None,
                updated_at_ms: now_ms(),
            }
        }
        Err(err_msg) => {
            let is_auth = err_msg.starts_with("AUTH_REQUIRED:")
                || err_msg.contains("HTTP 401")
                || err_msg.contains("HTTP 403")
                || err_msg.to_lowercase().contains("unauthorized");
            let clean_err = err_msg
                .trim_start_matches("AUTH_REQUIRED:")
                .trim()
                .to_string();
            CachedMcpEntry {
                fingerprint: fp,
                session_id: None,
                resolved_transport: mode.to_string(),
                server_instructions: String::new(),
                tools: Vec::new(),
                health_status: if is_auth {
                    "auth_required".to_string()
                } else {
                    "error".to_string()
                },
                health_error: Some(clean_err),
                updated_at_ms: now_ms(),
            }
        }
    }
}

fn apply_http_auth_headers(
    mut req: reqwest::RequestBuilder,
    headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    let has_explicit_auth = headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("authorization"));
    for (hk, hv) in headers {
        let k_trim = hk.trim();
        let v_trim = hv.trim();
        if k_trim.is_empty() || v_trim.is_empty() {
            continue;
        }
        if !has_explicit_auth
            && (k_trim.eq_ignore_ascii_case("token")
                || k_trim.eq_ignore_ascii_case("bearer")
                || k_trim.eq_ignore_ascii_case("api_key"))
        {
            let auth_val = if v_trim.starts_with("Bearer ") || v_trim.starts_with("Basic ") {
                v_trim.to_string()
            } else {
                format!("Bearer {v_trim}")
            };
            req = req.header("Authorization", auth_val);
        } else if k_trim.eq_ignore_ascii_case("authorization")
            && !v_trim.contains(' ')
            && !v_trim.is_empty()
        {
            req = req.header("Authorization", format!("Bearer {v_trim}"));
        } else {
            req = req.header(k_trim, v_trim);
        }
    }
    req
}

fn extract_instructions_from_init(init_res: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(desc) = init_res
        .get("serverInfo")
        .and_then(|s| s.get("description"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(desc.to_string());
    }
    if let Some(instr) = init_res
        .get("instructions")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Keep concise first ~550 chars of server workflow instructions so prompt stays lean & crisp
        let snippet: String = instr.chars().take(550).collect();
        parts.push(snippet);
    }
    parts.join(" | ")
}

/// Checks if a JSON-RPC value matches `expected_id` (or is the sole JSON-RPC result/error frame).
fn match_jsonrpc_frame(val: &Value, expected_id: i64) -> Option<Result<Value, String>> {
    if let Some(arr) = val.as_array() {
        for item in arr {
            if let Some(hit) = match_jsonrpc_frame(item, expected_id) {
                return Some(hit);
            }
        }
        return None;
    }
    let id_matches = match val.get("id") {
        Some(Value::Number(n)) => n.as_i64() == Some(expected_id),
        Some(Value::String(s)) => s == &expected_id.to_string(),
        _ => false,
    };
    if !id_matches {
        return None;
    }
    if let Some(err) = val.get("error") {
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| err.to_string());
        return Some(Err(format!("MCP RPC 错误: {msg}")));
    }
    Some(Ok(val.get("result").cloned().unwrap_or(Value::Null)))
}

/// Incrementally scans an SSE buffer (`event: message\ndata: {...}\n\n`) for the JSON-RPC response matching `expected_id`.
fn scan_sse_buffer_for_id(buf: &str, expected_id: i64, allow_unterminated: bool) -> Option<Result<Value, String>> {
    let normalized = buf.replace("\r\n", "\n");
    let blocks: Vec<&str> = if allow_unterminated {
        normalized.split("\n\n").collect()
    } else {
        let mut parts: Vec<&str> = normalized.split("\n\n").collect();
        // Keep trailing incomplete block until `\n\n` arrives
        let _ = parts.pop();
        parts
    };

    for block in blocks {
        let mut data_lines = Vec::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
        if data_lines.is_empty() {
            continue;
        }
        let joined = data_lines.join("\n");
        if let Ok(val) = serde_json::from_str::<Value>(joined.trim()) {
            if let Some(outcome) = match_jsonrpc_frame(&val, expected_id) {
                return Some(outcome);
            }
        }
    }
    None
}

/// Reads a Streamable HTTP response (`application/json` OR `text/event-stream`), returning as soon as `expected_id` arrives!
async fn read_streamable_http_rpc_response(
    mut resp: reqwest::Response,
    expected_id: Option<i64>,
) -> Result<(Option<String>, Value), String> {
    let status = resp.status();
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(format!(
            "AUTH_REQUIRED: 该 MCP 服务需要身份认证（HTTP {}），请在下方配置认证 Token / Header 或使用已授权的专属链接",
            status.as_u16()
        ));
    }
    if status.as_u16() == 202 || expected_id.is_none() {
        return Ok((session_id, Value::Null));
    }
    if !status.is_success() {
        let err_body = resp.text().await.unwrap_or_default();
        let snippet: String = err_body.chars().take(180).collect();
        if (status.as_u16() == 400 || status.as_u16() == 404)
            && snippet.to_lowercase().contains("session")
        {
            return Err(format!("SESSION_EXPIRED: HTTP {} {}", status.as_u16(), snippet));
        }
        return Err(format!(
            "HTTP {}{}",
            status.as_u16(),
            if snippet.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", snippet.trim())
            }
        ));
    }

    let req_id = expected_id.unwrap_or(1);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if content_type.contains("text/event-stream") {
        let mut sse_buf = String::new();
        let read_fut = async {
            while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
                sse_buf.push_str(&String::from_utf8_lossy(&chunk));
                if let Some(hit) = scan_sse_buffer_for_id(&sse_buf, req_id, false) {
                    return hit.map(|v| (session_id.clone(), v));
                }
                if sse_buf.len() > 4 * 1024 * 1024 {
                    break;
                }
            }
            if let Some(hit) = scan_sse_buffer_for_id(&sse_buf, req_id, true) {
                return hit.map(|v| (session_id.clone(), v));
            }
            Err("MCP SSE 流未返回对应的 JSON-RPC 响应".to_string())
        };

        return tokio::time::timeout(Duration::from_secs(18), read_fut)
            .await
            .map_err(|_| "读取 MCP SSE 流响应超时".to_string())?;
    }

    // Standard JSON (or mislabeled SSE body)
    let raw_bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let raw_text = String::from_utf8_lossy(&raw_bytes);
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        return Ok((session_id, Value::Null));
    }
    if trimmed.starts_with("event:") || trimmed.starts_with("data:") {
        if let Some(hit) = scan_sse_buffer_for_id(trimmed, req_id, true) {
            return hit.map(|v| (session_id, v));
        }
    }
    let body: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("解析 MCP 响应失败: {e}"))?;
    if let Some(hit) = match_jsonrpc_frame(&body, req_id) {
        return hit.map(|v| (session_id, v));
    }
    if let Some(err) = body.get("error") {
        return Err(format!("MCP RPC 错误: {err}"));
    }
    Ok((session_id, body.get("result").cloned().unwrap_or(Value::Null)))
}

/// Full Streamable HTTP (`2025-03-26`) handshake: `initialize` -> `notifications/initialized` -> `tools/list`.
async fn mcp_streamable_http_init_and_list(
    srv: &McpServerConfigDto,
) -> Result<(Option<String>, String, Value), String> {
    let init_payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "keepdays-pet-agent", "version": "1.0.0" }
        }
    });

    let mut last_err = String::new();
    let mut init_ok: Option<(Option<String>, Value)> = None;

    for attempt in 0..2u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        let req = apply_http_auth_headers(
            MCP_HTTP_CLIENT
                .post(srv.endpoint.trim())
                .header("Accept", "application/json, text/event-stream")
                .header("Content-Type", "application/json")
                .json(&init_payload),
            &srv.headers,
        );
        match req.send().await {
            Ok(resp) => match read_streamable_http_rpc_response(resp, Some(1)).await {
                Ok(pair) => {
                    init_ok = Some(pair);
                    break;
                }
                Err(e) => {
                    last_err = e;
                    if last_err.starts_with("AUTH_REQUIRED:")
                        || last_err.contains("HTTP 404")
                        || last_err.contains("HTTP 405")
                    {
                        break;
                    }
                }
            },
            Err(e) => {
                last_err = format!("网络连接失败: {e}");
            }
        }
    }

    let (session_id, init_val) = init_ok.ok_or(last_err)?;
    let instructions = extract_instructions_from_init(&init_val);

    // Best-effort `notifications/initialized` (stateless servers may ignore or return 202/200)
    let notif_payload = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let mut notif_req = apply_http_auth_headers(
        MCP_HTTP_CLIENT
            .post(srv.endpoint.trim())
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .header("Mcp-Protocol-Version", MCP_PROTOCOL_VERSION)
            .json(&notif_payload),
        &srv.headers,
    );
    if let Some(ref sid) = session_id {
        notif_req = notif_req.header("Mcp-Session-Id", sid);
    }
    let _ = notif_req.send().await;

    let list_val =
        mcp_streamable_http_call(srv, session_id.as_deref(), "tools/list", json!({})).await?;
    Ok((session_id, instructions, list_val))
}

async fn mcp_streamable_http_call(
    srv: &McpServerConfigDto,
    session_id: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": method,
        "params": params,
    });

    let mut last_err = String::new();
    for attempt in 0..2u32 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        let mut req = apply_http_auth_headers(
            MCP_HTTP_CLIENT
                .post(srv.endpoint.trim())
                .header("Accept", "application/json, text/event-stream")
                .header("Content-Type", "application/json")
                .header("Mcp-Protocol-Version", MCP_PROTOCOL_VERSION)
                .json(&payload),
            &srv.headers,
        );
        if let Some(sid) = session_id {
            req = req.header("Mcp-Session-Id", sid);
        }

        match req.send().await {
            Ok(resp) => match read_streamable_http_rpc_response(resp, Some(2)).await {
                Ok((_, val)) => return Ok(val),
                Err(e) => {
                    last_err = e;
                    if last_err.starts_with("AUTH_REQUIRED:")
                        || last_err.starts_with("SESSION_EXPIRED:")
                    {
                        break;
                    }
                }
            },
            Err(e) => {
                last_err = format!("网络连接失败: {e}");
            }
        }
    }
    Err(last_err)
}

fn resolve_sse_endpoint_url(base_sse_url: &str, raw_endpoint: &str) -> String {
    let ep = raw_endpoint.trim();
    if ep.starts_with("http://") || ep.starts_with("https://") {
        // Prevent cross-origin SSRF / credential forwarding if remote SSE server returns a different origin!
        let origin_of = |u: &str| -> Option<String> {
            let sep = u.find("://")?;
            let rest = &u[sep + 3..];
            let host_end = rest.find('/').unwrap_or(rest.len());
            Some(u[..sep + 3 + host_end].to_ascii_lowercase())
        };
        if origin_of(ep) == origin_of(base_sse_url) {
            return ep.to_string();
        }
        return String::new();
    }
    // Extract scheme://authority from base_sse_url
    if let Some(scheme_sep) = base_sse_url.find("://") {
        let after_scheme = &base_sse_url[scheme_sep + 3..];
        if let Some(slash_idx) = after_scheme.find('/') {
            let origin = &base_sse_url[..scheme_sep + 3 + slash_idx];
            if ep.starts_with('/') {
                return format!("{origin}{ep}");
            }
            let base_dir = base_sse_url
                .rsplit_once('/')
                .map(|(d, _)| d)
                .unwrap_or(origin);
            return format!("{base_dir}/{ep}");
        }
        return format!("{base_sse_url}/{}", ep.trim_start_matches('/'));
    }
    ep.to_string()
}

/// Legacy `2024-11-05` HTTP+SSE MCP Transport (`/sse` -> `event: endpoint` -> POST session URL).
/// Supports older remote MCP gateways (Composio, Zapier `/sse`, Higress, FastMCP SSE).
async fn mcp_legacy_sse_call(
    srv: &McpServerConfigDto,
    method: &str,
    params: Value,
) -> Result<(String, Value), String> {
    let mut sse_resp = apply_http_auth_headers(
        MCP_HTTP_CLIENT
            .get(srv.endpoint.trim())
            .header("Accept", "text/event-stream"),
        &srv.headers,
    )
    .send()
    .await
    .map_err(|e| format!("SSE 连接失败: {e}"))?;

    let status = sse_resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(format!(
            "AUTH_REQUIRED: 该 MCP 服务需要身份认证（HTTP {}），请配置认证 Token / Header",
            status.as_u16()
        ));
    }
    if !status.is_success() {
        return Err(format!("MCP SSE 握手返回 HTTP {}", status.as_u16()));
    }

    let mut sse_buf = String::new();
    let post_url = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = sse_resp.chunk().await.map_err(|e| e.to_string())? {
            sse_buf.push_str(&String::from_utf8_lossy(&chunk));
            let norm = sse_buf.replace("\r\n", "\n");
            let mut blocks: Vec<&str> = norm.split("\n\n").collect();
            let rest = blocks.pop().unwrap_or("").to_string();
            for block in blocks {
                let mut is_endpoint = false;
                let mut data_val = String::new();
                for line in block.lines() {
                    if let Some(ev) = line.strip_prefix("event:") {
                        if ev.trim() == "endpoint" {
                            is_endpoint = true;
                        }
                    } else if let Some(d) = line.strip_prefix("data:") {
                        data_val.push_str(d.trim());
                    }
                }
                if is_endpoint && !data_val.is_empty() {
                    sse_buf = rest;
                    return Ok(resolve_sse_endpoint_url(srv.endpoint.trim(), &data_val));
                }
            }
        }
        Err("MCP SSE 未返回 endpoint 会话地址".to_string())
    })
    .await
    .map_err(|_| "等待 MCP SSE endpoint 超时".to_string())??;

    // 1. Send `initialize` (id=1)
    let init_payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "keepdays-pet-agent", "version": "1.0.0" }
        }
    });
    let post_init = apply_http_auth_headers(
        MCP_HTTP_CLIENT.post(&post_url).json(&init_payload),
        &srv.headers,
    )
    .send()
    .await
    .map_err(|e| e.to_string())?;

    let init_val = if let Ok((_, direct)) = read_streamable_http_rpc_response(post_init, Some(1)).await {
        if !direct.is_null() {
            direct
        } else {
            await_id_from_sse_stream(&mut sse_resp, &mut sse_buf, 1).await?
        }
    } else {
        await_id_from_sse_stream(&mut sse_resp, &mut sse_buf, 1).await?
    };
    let instructions = extract_instructions_from_init(&init_val);

    // 2. Send `notifications/initialized`
    let _ = apply_http_auth_headers(
        MCP_HTTP_CLIENT
            .post(&post_url)
            .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
        &srv.headers,
    )
    .send()
    .await;

    // 3. Send target `method` (id=2)
    let call_payload = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": method,
        "params": params,
    });
    let post_call = apply_http_auth_headers(
        MCP_HTTP_CLIENT.post(&post_url).json(&call_payload),
        &srv.headers,
    )
    .send()
    .await
    .map_err(|e| e.to_string())?;

    let call_val = if let Ok((_, direct)) = read_streamable_http_rpc_response(post_call, Some(2)).await {
        if !direct.is_null() {
            direct
        } else {
            await_id_from_sse_stream(&mut sse_resp, &mut sse_buf, 2).await?
        }
    } else {
        await_id_from_sse_stream(&mut sse_resp, &mut sse_buf, 2).await?
    };

    Ok((instructions, call_val))
}

async fn await_id_from_sse_stream(
    sse_resp: &mut reqwest::Response,
    sse_buf: &mut String,
    expected_id: i64,
) -> Result<Value, String> {
    if let Some(hit) = scan_sse_buffer_for_id(sse_buf, expected_id, false) {
        return hit;
    }
    tokio::time::timeout(Duration::from_secs(16), async {
        while let Some(chunk) = sse_resp.chunk().await.map_err(|e| e.to_string())? {
            sse_buf.push_str(&String::from_utf8_lossy(&chunk));
            if let Some(hit) = scan_sse_buffer_for_id(sse_buf, expected_id, false) {
                return hit;
            }
        }
        if let Some(hit) = scan_sse_buffer_for_id(sse_buf, expected_id, true) {
            return hit;
        }
        Err("MCP SSE 会话提前断开".to_string())
    })
    .await
    .map_err(|_| "等待 MCP SSE 响应超时".to_string())?
}

fn build_enriched_stdio_path() -> String {
    let mut dirs = vec![
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/sbin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
    ];
    if let Ok(home) = std::env::var("HOME") {
        dirs.insert(0, format!("{home}/.local/bin"));
        dirs.insert(1, format!("{home}/.cargo/bin"));
    }
    if let Ok(existing) = std::env::var("PATH") {
        for part in existing.split(':') {
            if !part.is_empty() && !dirs.iter().any(|d| d == part) {
                dirs.push(part.to_string());
            }
        }
    }
    dirs.join(":")
}

/// Local `stdio` MCP Transport (`npx`, `uvx`, `node`, `python3`, etc. — e.g. Gmail AutoAuth MCP, GitHub MCP).
async fn mcp_stdio_call(
    srv: &McpServerConfigDto,
    method: &str,
    params: Value,
) -> Result<(String, Value), String> {
    if std::env::var("ALLOW_STDIO_MCP")
        .map(|v| v.eq_ignore_ascii_case("false") || v == "0")
        .unwrap_or(false)
    {
        return Err(
            "当前服务器已禁用本地命令行 (stdio) MCP，请改用远程 HTTP/SSE MCP 服务".to_string(),
        );
    }

    let raw_ep = srv.endpoint.trim();
    let (program, final_args): (String, Vec<String>) =
        if srv.args.is_empty() && raw_ep.contains(' ') {
            let mut parts: Vec<String> = raw_ep
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let prog = parts.remove(0);
            (prog, parts)
        } else {
            (raw_ep.to_string(), srv.args.clone())
        };

    if program.is_empty() {
        return Err("MCP 命令不能为空".to_string());
    }

    let safe_cwd = std::env::temp_dir().join("keepdays-mcp-sandbox");
    let safe_home = safe_cwd.join("home");
    let _ = std::fs::create_dir_all(&safe_home);

    let mut cmd = Command::new(&program);
    // Strip ALL KeepDays daemon secrets (`JWT_SECRET`, `S3_SECRET_KEY`, `ADMIN_API_KEY`, `DATABASE_URL`)!
    cmd.env_clear()
        .args(&final_args)
        .current_dir(&safe_cwd)
        .env("PATH", build_enriched_stdio_path())
        .env("HOME", &safe_home)
        .env("TMPDIR", std::env::temp_dir())
        .env("LANG", "C.UTF-8")
        .env("TERM", "dumb")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for safe_sys_key in ["SSL_CERT_FILE", "SSL_CERT_DIR", "NODE_EXTRA_CA_CERTS", "HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY"] {
        if let Ok(val) = std::env::var(safe_sys_key) {
            cmd.env(safe_sys_key, val);
        }
    }

    #[cfg(unix)]
    {
        #[allow(unused_imports)]
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // Inject user-provided env vars (stored in `headers` for stdio servers, e.g. GMAIL_CLIENT_ID / API keys)
    for (k, v) in &srv.headers {
        let kt = k.trim();
        if !kt.is_empty() {
            cmd.env(kt, v.trim());
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动本地 MCP 命令 `{program}` 失败: {e}"))?;

    struct ProcessGroupKillGuard {
        #[cfg(unix)]
        pgid: Option<i32>,
    }
    impl Drop for ProcessGroupKillGuard {
        fn drop(&mut self) {
            #[cfg(unix)]
            if let Some(pgid) = self.pgid {
                if pgid > 1 {
                    unsafe {
                        let _ = libc::kill(-pgid, libc::SIGKILL);
                    }
                }
            }
        }
    }
    let _pg_guard = ProcessGroupKillGuard {
        #[cfg(unix)]
        pgid: child.id().and_then(|id| i32::try_from(id).ok()),
    };

    let mut stdin = child.stdin.take().ok_or("stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let stderr = child.stderr.take();
    let mut reader = BufReader::new(stdout).lines();

    // Drain `stderr` concurrently so >64KB startup/debug logs NEVER deadlock the child process pipe!
    let stderr_task = tokio::spawn(async move {
        let mut captured = Vec::new();
        if let Some(se) = stderr {
            let mut head = se.take(4096);
            let _ = head.read_to_end(&mut captured).await;
            let mut rest = head.into_inner();
            let _ = tokio::io::copy(&mut rest, &mut tokio::io::sink()).await;
        }
        String::from_utf8_lossy(&captured).trim().to_string()
    });

    let init_msg = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "keepdays-pet-agent", "version": "1.0.0" }
        }
    });

    let exchange = async {
        // Step 1: Send `initialize` and WAIT for `id == 0` response before sending `tools/list` or `tools/call`!
        stdin
            .write_all(format!("{init_msg}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;

        let mut init_val = Value::Null;
        let mut got_init = false;
        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                if let Some(hit) = match_jsonrpc_frame(&v, 0) {
                    init_val = hit?;
                    got_init = true;
                    break;
                }
            }
        }
        if !got_init {
            return Err("MCP stdio 进程在握手阶段提前退出".to_string());
        }

        let instructions = extract_instructions_from_init(&init_val);

        // Step 2: Send `notifications/initialized` + target request (`id == 1`)
        let notif_msg = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let call_msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        stdin
            .write_all(format!("{notif_msg}\n{call_msg}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())?;

        while let Ok(Some(line)) = reader.next_line().await {
            if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                if let Some(hit) = match_jsonrpc_frame(&v, 1) {
                    return hit.map(|res| (instructions, res));
                }
            }
        }
        Err("MCP stdio 进程未返回工具结果".to_string())
    };

    match tokio::time::timeout(Duration::from_secs(25), exchange).await {
        Ok(Ok(pair)) => {
            stderr_task.abort();
            Ok(pair)
        }
        Ok(Err(err_msg)) => {
            drop(_pg_guard);
            let err_tail = tokio::time::timeout(Duration::from_millis(150), stderr_task)
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default();
            if err_tail.is_empty() {
                Err(err_msg)
            } else {
                let short: String = err_tail.chars().take(200).collect();
                Err(format!("{err_msg} ({short})"))
            }
        }
        Err(_) => {
            stderr_task.abort();
            Err("本地 MCP stdio 命令执行超时（首次 npx 下载依赖可能较慢，可点刷新重试）".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_streamable_http_sse_event_frame() {
        let raw_sse = "event: message\r\ndata: {\"result\":{\"tools\":[{\"name\":\"openmeteo_get_forecast\"}]},\"jsonrpc\":\"2.0\",\"id\":2}\r\n\r\n";
        let parsed = scan_sse_buffer_for_id(raw_sse, 2, false)
            .expect("should match id 2")
            .expect("should be Ok");
        let tools = parsed.get("tools").and_then(|t| t.as_array()).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "openmeteo_get_forecast");
    }

    #[test]
    fn resolves_relative_legacy_sse_endpoint() {
        assert_eq!(
            resolve_sse_endpoint_url("https://mcp.example.com/v1/sse", "/messages?sessionId=abc"),
            "https://mcp.example.com/messages?sessionId=abc"
        );
        assert_eq!(
            resolve_sse_endpoint_url("https://mcp.example.com/v1/sse", "messages?sid=1"),
            "https://mcp.example.com/v1/messages?sid=1"
        );
    }
}
