use serde_json::{json, Value};
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use crate::{
    agent::{
        a2ui::{validate_and_sanitize_surface, A2uiSurfacePayload, ALLOWED_A2UI_COMPONENTS},
        mcp::{discover_mcp_tools, invoke_mcp_tool},
        memory::{apply_ltm_git_commit, segment_for_bm25, user_scope, LtmDiffOp},
        provider::ToolSpecWire,
        sandbox::VirtualFs,
    },
    domains::{
        fridge::{BoardDoc, BoardNote, BoardTodo, FridgeDoc, FridgeSlip, MenuDish, MenuDoc},
        media::resolve_asset_urls,
        pet::model::{PetLogEntry, PetStateDoc},
        photo::PhotoPostDoc,
    },
    infra::dict::dim,
    state::AppState,
    time_bj::{now_beijing, now_ms},
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub struct ToolExecContext {
    pub state: AppState,
    pub space_id: i64,
    pub user_id: i64,
    pub session_id: String,
    vfs_cell: tokio::sync::OnceCell<VirtualFs>,
}

impl ToolExecContext {
    pub fn new(state: AppState, space_id: i64, user_id: i64, session_id: String) -> Self {
        Self {
            state,
            space_id,
            user_id,
            session_id,
            vfs_cell: tokio::sync::OnceCell::new(),
        }
    }

    /// Lazily initializes `VirtualFs` (`AgentBuilder` + `LocalTerminalActor`) ONLY on turns
    /// that actually invoke sandbox file/bash tools, keeping regular chat turns 0-allocation!
    pub async fn vfs(&self) -> Result<&VirtualFs, String> {
        self.vfs_cell
            .get_or_try_init(|| {
                VirtualFs::ensure_for(&self.state.cfg.agent_fs_dir, self.space_id, self.user_id)
            })
            .await
    }
}

pub struct ToolExecOutput {
    pub model_text: String,
    pub display_summary: String,
    pub emitted_surface: Option<A2uiSurfacePayload>,
}

/// Unified `grok-build` style extensible `AgentTool` trait (`Mode A`).
/// Adding any new KeepDays feature later is literally implementing `AgentTool` and calling `registry.register(...)`!
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn domain(&self) -> &'static str;
    fn always_loaded(&self) -> bool {
        false
    }
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> Value;
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>>;
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, Arc<dyn AgentTool>>>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_builtin_tools()
    }
}

impl ToolRegistry {
    pub fn with_builtin_tools() -> Self {
        let reg = Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
        };
        // 1. Always-loaded sandboxed core tools + A2UI + `search_tool` / `use_tool`
        reg.register(Arc::new(BashTool));
        reg.register(Arc::new(ReadFileTool));
        reg.register(Arc::new(EditFileTool));
        reg.register(Arc::new(GrepFilesTool));
        reg.register(Arc::new(A2uiSurfaceTool));
        reg.register(Arc::new(SearchToolMeta));
        reg.register(Arc::new(UseToolMeta));

        // 2. KeepDays Platform Domain Tools (discovered via `search_tool`, invoked via `use_tool` or directly)
        reg.register(Arc::new(FridgeOverviewTool));
        reg.register(Arc::new(FridgePostNoteTool));
        reg.register(Arc::new(FridgeManageTodoTool));
        reg.register(Arc::new(FridgePinMenuTool));
        reg.register(Arc::new(FridgeUpsertDishTool));
        reg.register(Arc::new(AlbumSearchPhotosTool));
        reg.register(Arc::new(AlbumStatsSummaryTool));
        reg.register(Arc::new(PetActAndEmoteTool));
        reg.register(Arc::new(PetGetVitalsTool));
        reg.register(Arc::new(MemoryCommitHistoryTool));
        reg.register(Arc::new(MemoryResolveClarificationTool));

        reg
    }

    /// Plug-and-play registration hook for future KeepDays modules.
    pub fn register(&self, tool: Arc<dyn AgentTool>) {
        if let Ok(mut map) = self.tools.write() {
            map.insert(tool.name().to_string(), tool);
        }
    }

    /// Returns the static always-loaded tool definitions so LLM Prefix Cache stays 100% intact.
    pub fn always_loaded_specs(&self) -> Vec<ToolSpecWire> {
        let Ok(map) = self.tools.read() else {
            return Vec::new();
        };
        let mut specs: Vec<ToolSpecWire> = map
            .values()
            .filter(|t| t.always_loaded())
            .map(|t| ToolSpecWire {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    pub async fn dispatch(
        &self,
        ctx: &ToolExecContext,
        name: &str,
        args: Value,
    ) -> Result<ToolExecOutput, String> {
        if name == "search_tool" {
            return self.execute_search_tool(ctx, args).await;
        }
        if name == "use_tool" {
            let target = args
                .get("tool_name")
                .or_else(|| args.get("name"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "`use_tool` 缺少 `tool_name` 参数".to_string())?
                .to_string();
            if target == "use_tool" || target == "search_tool" {
                return Err("请勿通过 `use_tool` 递归调用元工具".to_string());
            }
            let inner_args = args
                .get("arguments")
                .or_else(|| args.get("input"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            return Box::pin(self.dispatch_non_meta(ctx, &target, inner_args)).await;
        }
        self.dispatch_non_meta(ctx, name, args).await
    }

    async fn dispatch_non_meta(
        &self,
        ctx: &ToolExecContext,
        name: &str,
        args: Value,
    ) -> Result<ToolExecOutput, String> {
        let maybe_tool = self
            .tools
            .read()
            .ok()
            .and_then(|m| m.get(name).cloned());
        if let Some(tool) = maybe_tool {
            return tool.run(ctx, args).await;
        }
        if name.starts_with("mcp__") {
            let (summary, res) = invoke_mcp_tool(&ctx.state, ctx.space_id, name, args).await?;
            return Ok(ToolExecOutput {
                display_summary: summary,
                model_text: res,
                emitted_surface: None,
            });
        }
        // Also allow matching bare remote MCP tool names (e.g. `openmeteo_search_locations` or `send_email`)
        // in case the LLM copied the raw tool name from MCP instructions without the `mcp__<srv>__` prefix!
        match invoke_mcp_tool(&ctx.state, ctx.space_id, name, args).await {
            Ok((summary, res)) => Ok(ToolExecOutput {
                display_summary: summary,
                model_text: res,
                emitted_surface: None,
            }),
            Err(e) if !e.starts_with("未找到 MCP 工具") => Err(e),
            Err(_) => Err(format!(
                "未知工具 `{name}`；请先用 `search_tool` 搜索可用工具名"
            )),
        }
    }

    /// Ported from `xai-grok-tools/src/implementations/search_tool`:
    /// BM25-style keyword + CJK bigram + bilingual domain expansion across registered domain tools & live MCP servers.
    async fn execute_search_tool(
        &self,
        ctx: &ToolExecContext,
        args: Value,
    ) -> Result<ToolExecOutput, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(12)
            .clamp(1, 25) as usize;

        let expanded_query = expand_bilingual_tool_query(&query.to_lowercase());
        let q_tokens: HashSet<String> = segment_for_bm25(&expanded_query)
            .split_whitespace()
            .map(str::to_string)
            .collect();

        let mut candidates: Vec<(f64, Value)> = Vec::new();
        let local_tools: Vec<Arc<dyn AgentTool>> = self
            .tools
            .read()
            .map(|m| m.values().filter(|t| !t.always_loaded()).cloned().collect())
            .unwrap_or_default();

        for t in &local_tools {
            let hay = format!("{} {} {}", t.name(), t.domain(), t.description()).to_lowercase();
            let doc_tokens: HashSet<String> = segment_for_bm25(&hay)
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let overlap = if q_tokens.is_empty() {
                1.0
            } else {
                q_tokens.intersection(&doc_tokens).count() as f64
                    + if !query.is_empty() && hay.contains(&query.to_lowercase()) {
                        3.0
                    } else {
                        0.0
                    }
            };
            if q_tokens.is_empty() || overlap > 0.0 {
                candidates.push((
                    overlap,
                    json!({
                        "tool_name": t.name(),
                        "domain": t.domain(),
                        "description": t.description(),
                        "input_schema": t.parameters_schema(),
                    }),
                ));
            }
        }

        // Include live MCP tools (`Mode C`)
        let discovered_mcp = discover_mcp_tools(&ctx.state, ctx.space_id).await;
        for mcp_t in &discovered_mcp {
            let hay = format!(
                "{} {} {} {}",
                mcp_t.qualified_name, mcp_t.remote_tool_name, mcp_t.server_name, mcp_t.description
            )
            .to_lowercase();
            let doc_tokens: HashSet<String> = segment_for_bm25(&hay)
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let overlap = if q_tokens.is_empty() {
                1.5
            } else {
                q_tokens.intersection(&doc_tokens).count() as f64
                    + if !query.is_empty() && hay.contains(&query.to_lowercase()) {
                        4.0
                    } else {
                        0.0
                    }
            };
            if q_tokens.is_empty() || overlap > 0.0 {
                candidates.push((
                    overlap,
                    json!({
                        "tool_name": mcp_t.qualified_name,
                        "domain": format!("mcp:{}", mcp_t.server_name),
                        "description": mcp_t.description,
                        "input_schema": mcp_t.input_schema,
                    }),
                ));
            }
        }

        // If query matched nothing specifically, return MCP tools FIRST + platform tools so external MCPs are never missed!
        if candidates.is_empty() {
            for mcp_t in &discovered_mcp {
                candidates.push((
                    0.5,
                    json!({
                        "tool_name": mcp_t.qualified_name,
                        "domain": format!("mcp:{}", mcp_t.server_name),
                        "description": mcp_t.description,
                        "input_schema": mcp_t.input_schema,
                    }),
                ));
            }
            for t in &local_tools {
                candidates.push((
                    0.1,
                    json!({
                        "tool_name": t.name(),
                        "domain": t.domain(),
                        "description": t.description(),
                        "input_schema": t.parameters_schema(),
                    }),
                ));
            }
        }

        candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let results: Vec<Value> = candidates.into_iter().take(limit).map(|(_, v)| v).collect();
        let count = results.len();
        Ok(ToolExecOutput {
            display_summary: format!("检索到 {count} 个可用扩展工具"),
            model_text: serde_json::to_string_pretty(&json!({
                "matched_tools": results,
                "usage_hint": "使用 `use_tool` 传入 `tool_name` 与 `arguments` 即可调用上述任意工具。"
            }))
            .unwrap_or_default(),
            emitted_surface: None,
        })
    }
}

/// Bridges Chinese natural queries (`天气`/`邮件`/`日历`/`地图`...) to English MCP tool names & schemas.
fn expand_bilingual_tool_query(q: &str) -> String {
    const SYNONYMS: &[(&[&str], &str)] = &[
        (
            &["天气", "气温", "温度", "下雨", "降雨", "预报", "空气", "风力", "湿度", "气象", "weather", "meteo"],
            "weather forecast openmeteo temperature location air quality precipitation wind",
        ),
        (
            &["邮件", "邮箱", "收件", "发信", "草稿", "gmail", "email", "mail"],
            "gmail email mail inbox message thread send draft search_emails",
        ),
        (
            &["日历", "日程", "会议", "安排", "calendar", "schedule"],
            "calendar event schedule meeting reminder",
        ),
        (
            &["搜索", "网页", "新闻", "抓取", "联网", "search", "web"],
            "search web fetch scrape browser url query",
        ),
        (
            &["地图", "路线", "导航", "周边", "地点", "经纬度", "map", "amap"],
            "map location geocode route directions place search_locations",
        ),
        (
            &["笔记", "文档", "知识库", "notion", "doc"],
            "notion page database document note wiki",
        ),
        (
            &["代码", "仓库", "提交", "github", "git"],
            "github git repository issue pull_request commit file",
        ),
    ];

    let mut extra = String::from(q);
    for (triggers, expansion) in SYNONYMS {
        if triggers.iter().any(|kw| q.contains(kw)) {
            extra.push(' ');
            extra.push_str(expansion);
        }
    }
    extra
}

// ============================================================================
// 1. Always-Loaded Core Tools (`bash`, `read_file`, `edit_file`, `grep_files`, `a2ui_surface`, `search_tool`, `use_tool`)
// ============================================================================

struct BashTool;
impl AgentTool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }
    fn domain(&self) -> &'static str {
        "sandbox"
    }
    fn always_loaded(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "在隔离沙箱中执行 Shell 命令。默认工作目录固定为当前对话人的私有目录 `~/private`，双人共享目录为 `~/shared`（环境变量 `$SHARED_DIR`）。不可访问宿主机外部文件。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的 bash 命令" }
            },
            "required": ["command"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "缺少 `command` 参数".to_string())?;
            let vfs = ctx.vfs().await?;
            let out = vfs.run_bash(cmd).await?;
            Ok(ToolExecOutput {
                display_summary: "已在安全沙箱执行脚本".to_string(),
                model_text: out,
                emitted_surface: None,
            })
        })
    }
}

struct ReadFileTool;
impl AgentTool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn domain(&self) -> &'static str {
        "sandbox"
    }
    fn always_loaded(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "读取沙箱工作区文件（支持相对路径、`~/private/...` 或 `~/shared/...`）。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "虚拟路径，如 `notes.md` 或 `~/shared/plan.md`" },
                "start_line": { "type": "integer" },
                "end_line": { "type": "integer" }
            },
            "required": ["path"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let raw_path = args
                .get("path")
                .or_else(|| args.get("target_file"))
                .or_else(|| args.get("file_path"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "缺少 `path` 参数".to_string())?;
            let vfs = ctx.vfs().await?;
            let (real_path, vlabel) = vfs.resolve_virtual_path(raw_path)?;

            let mut kparams = serde_json::Map::new();
            kparams.insert(
                "target_file".to_string(),
                Value::String(real_path.to_string_lossy().into_owned()),
            );
            if let Some(s) = args.get("start_line").and_then(|v| v.as_i64()) {
                kparams.insert("offset".to_string(), json!(s.max(1)));
                if let Some(e) = args.get("end_line").and_then(|v| v.as_i64()) {
                    if e >= s {
                        kparams.insert("limit".to_string(), json!((e - s + 1).max(1)));
                    }
                }
            }

            let out = vfs
                .call_kernel_tool("GrokBuild:read_file", Value::Object(kparams), "kd_read_call")
                .await?;

            Ok(ToolExecOutput {
                display_summary: format!("读取文件 {vlabel}"),
                model_text: format!("{out}\n[file: {vlabel} | cwd: ~/private | shared: ~/shared]"),
                emitted_surface: None,
            })
        })
    }
}

struct EditFileTool;
impl AgentTool for EditFileTool {
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn domain(&self) -> &'static str {
        "sandbox"
    }
    fn always_loaded(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "在 `~/private` 或 `~/shared` 工作区创建/修改文件。传 `content` 为整文件写入；传 `old_text` + `new_text` 为精确替换。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "如 `memo.md` 或 `~/shared/shopping.md`" },
                "content": { "type": "string", "description": "完整写入内容（新建或覆盖时使用）" },
                "old_text": { "type": "string", "description": "局部替换的目标原文本" },
                "new_text": { "type": "string", "description": "局部替换的新文本" }
            },
            "required": ["path"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let raw_path = args
                .get("path")
                .or_else(|| args.get("file_path"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "缺少 `path` 参数".to_string())?;
            let vfs = ctx.vfs().await?;
            let (real_path, vlabel) = vfs.resolve_virtual_path(raw_path)?;

            if let Some(old_t) = args
                .get("old_text")
                .or_else(|| args.get("old_string"))
                .and_then(|v| v.as_str())
            {
                let new_t = args
                    .get("new_text")
                    .or_else(|| args.get("new_string"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let kernel_out = vfs
                    .call_kernel_tool(
                        "GrokBuild:search_replace",
                        json!({
                            "file_path": real_path.to_string_lossy(),
                            "old_string": old_t,
                            "new_string": new_t,
                        }),
                        "kd_edit_call",
                    )
                    .await?;
                return Ok(ToolExecOutput {
                    display_summary: format!("已更新工作区文件 {vlabel}"),
                    model_text: format!("{kernel_out}\n[cwd: ~/private | shared: ~/shared]"),
                    emitted_surface: None,
                });
            } else if let Some(full) = args.get("content").and_then(|v| v.as_str()) {
                use xai_grok_tools::computer::types::AsyncFileSystem as _;
                let jailed_fs = xai_grok_tools::virtual_jail::JailedFs::new(vfs.jail().clone());
                jailed_fs
                    .write_file(&real_path, full.as_bytes())
                    .await
                    .map_err(|e| vfs.scrub_output(&e.to_string()))?;
            } else {
                return Err("请提供 `content` 或 `old_text` + `new_text`".to_string());
            }

            Ok(ToolExecOutput {
                display_summary: format!("已更新工作区文件 {vlabel}"),
                model_text: format!("成功保存 {vlabel}\n[cwd: ~/private | shared: ~/shared]"),
                emitted_surface: None,
            })
        })
    }
}

struct GrepFilesTool;
impl AgentTool for GrepFilesTool {
    fn name(&self) -> &'static str {
        "grep_files"
    }
    fn domain(&self) -> &'static str {
        "sandbox"
    }
    fn always_loaded(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "列出或搜索 `~/private` 与 `~/shared` 沙箱工作区内的文件名与文本内容。不传 `pattern` 则列出文件目录树。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "scope": { "type": "string", "enum": ["both", "private", "shared"], "default": "both" },
                "pattern": { "type": "string", "description": "可选：要检索的文本关键词" }
            }
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("both").to_string();
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let vfs = ctx.vfs().await?;

            // Try kernel `GrokBuild:grep` first when searching by pattern; fall back to `walk_search` if `rg` is absent on host
            if let Some(pat) = pattern {
                let mut kernel_chunks = Vec::new();
                if scope == "both" || scope == "private" {
                    if let Ok(out) = vfs
                        .call_kernel_tool(
                            "GrokBuild:grep",
                            json!({
                                "pattern": pat,
                                "path": vfs.private_root().to_string_lossy(),
                            }),
                            "kd_grep_priv",
                        )
                        .await
                    {
                        kernel_chunks.push(out);
                    }
                }
                if scope == "both" || scope == "shared" {
                    if let Ok(out) = vfs
                        .call_kernel_tool(
                            "GrokBuild:grep",
                            json!({
                                "pattern": pat,
                                "path": vfs.shared_root().to_string_lossy(),
                            }),
                            "kd_grep_shared",
                        )
                        .await
                    {
                        kernel_chunks.push(out);
                    }
                }
                if !kernel_chunks.is_empty() {
                    return Ok(ToolExecOutput {
                        display_summary: "检索沙箱工作区文件".to_string(),
                        model_text: format!(
                            "{}\n[cwd: ~/private | shared: ~/shared]",
                            kernel_chunks.join("\n")
                        ),
                        emitted_surface: None,
                    });
                }
            }

            let pat_lower = pattern.map(str::to_lowercase);
            let priv_root = vfs.private_root().to_path_buf();
            let shared_root = vfs.shared_root().to_path_buf();
            let hits = tokio::task::spawn_blocking(move || {
                let mut h = Vec::new();
                if scope == "both" || scope == "private" {
                    walk_search(&priv_root, "~/private", pat_lower.as_deref(), &mut h);
                }
                if scope == "both" || scope == "shared" {
                    walk_search(&shared_root, "~/shared", pat_lower.as_deref(), &mut h);
                }
                h
            })
            .await
            .unwrap_or_default();

            let body = if hits.is_empty() {
                "(工作区暂无匹配文件)".to_string()
            } else {
                hits.join("\n")
            };
            Ok(ToolExecOutput {
                display_summary: "检索沙箱工作区文件".to_string(),
                model_text: format!("{body}\n[cwd: ~/private | shared: ~/shared]"),
                emitted_surface: None,
            })
        })
    }
}

fn walk_search(root: &Path, prefix: &str, pattern: Option<&str>, out: &mut Vec<String>) {
    const MAX_READ_BYTES: u64 = 256 * 1024;
    fn rec(cur: &Path, root: &Path, prefix: &str, pattern: Option<&str>, out: &mut Vec<String>, depth: usize) {
        if out.len() >= 120 || depth > 12 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(cur) else {
            return;
        };
        for ent in entries.flatten() {
            let Ok(ft) = ent.file_type() else {
                continue;
            };
            // Skip symlinks (`private -> .` and `shared -> ../shared`) to prevent infinite cycle recursion!
            if ft.is_symlink() {
                continue;
            }
            let p = ent.path();
            if p.file_name().and_then(|s| s.to_str()) == Some(".tmp") {
                continue;
            }
            let rel = p
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let vpath = format!("{prefix}/{rel}");
            if ft.is_dir() {
                rec(&p, root, prefix, pattern, out, depth + 1);
            } else if let Some(pat) = pattern {
                if ent.metadata().map(|m| m.len() <= MAX_READ_BYTES).unwrap_or(false) {
                    if let Ok(txt) = std::fs::read_to_string(&p) {
                        for (idx, line) in txt.lines().enumerate() {
                            if line.to_lowercase().contains(pat) {
                                out.push(format!("{vpath}:{}: {}", idx + 1, line.trim()));
                                if out.len() >= 120 {
                                    return;
                                }
                            }
                        }
                    }
                }
            } else {
                out.push(vpath);
            }
        }
    }
    rec(root, root, prefix, pattern, out, 0);
}

struct A2uiSurfaceTool;
impl AgentTool for A2uiSurfaceTool {
    fn name(&self) -> &'static str {
        "a2ui_surface"
    }
    fn domain(&self) -> &'static str {
        "ui"
    }
    fn always_loaded(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "Google A2UI 动态界面下发工具：当适合用交互控件或数据图表与主人互动时（例如勾选菜单/待办、是/否表态、滑块调节、展示折线/柱状/饼环/雷达/日历热力图/指标表/照片条），自主编排原子组件并指定自动回传策略 `trigger`（`immediate` 点击即回传 | `debounce` 停顿自动回传 | `on_all_checked` 全部勾完回传 | `submit` 按钮提交）。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "surface": {
                    "type": "object",
                    "properties": {
                        "surface_id": { "type": "string" },
                        "title": { "type": "string" },
                        "root_id": { "type": "string" },
                        "data_model": { "type": "object", "description": "初始键值状态对象，供组件通过 bind_path（如 `/dishes`）双向绑定" },
                        "components": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "component": {
                                        "type": "string",
                                        "enum": ALLOWED_A2UI_COMPONENTS
                                    },
                                    "children": { "type": "array", "items": { "type": "string" } },
                                    "bind_path": { "type": "string" },
                                    "props": { "type": "object" }
                                },
                                "required": ["id", "component"]
                            }
                        },
                        "trigger": {
                            "type": "object",
                            "description": "回传策略，如 {\"mode\":\"immediate\",\"action\":\"yes_no\"} 或 {\"mode\":\"debounce\",\"delay_ms\":1400,\"action\":\"pick_menu\"} 或 {\"mode\":\"submit\",\"submit_label\":\"定好啦\",\"action\":\"submit_form\"}"
                        }
                    },
                    "required": ["surface_id", "root_id", "components"]
                }
            },
            "required": ["surface"]
        })
    }
    fn run<'a>(&'a self, _ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let raw_surf = args
                .get("surface")
                .cloned()
                .unwrap_or_else(|| args.clone());
            let parsed: A2uiSurfacePayload = serde_json::from_value(raw_surf)
                .map_err(|e| format!("A2UI surface JSON 结构非法: {e}"))?;
            let clean = validate_and_sanitize_surface(parsed)?;
            let title = clean
                .title
                .clone()
                .unwrap_or_else(|| "动态交互卡片".to_string());
            Ok(ToolExecOutput {
                display_summary: format!("已生成动态交互界面「{title}」"),
                model_text: format!(
                    "已向前端渲染 A2UI 交互组件 `{}`（共 {} 个原子节点），等待用户交互或继续回复简洁引导语。",
                    clean.surface_id,
                    clean.components.len()
                ),
                emitted_surface: Some(clean),
            })
        })
    }
}

struct SearchToolMeta;
impl AgentTool for SearchToolMeta {
    fn name(&self) -> &'static str {
        "search_tool"
    }
    fn domain(&self) -> &'static str {
        "meta"
    }
    fn always_loaded(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "按关键词检索 KeepDays 平台专属工具与外部 MCP 工具（涵盖域：`fridge` 冰箱便签/待办/家庭菜单、`album` 照片墙与回忆统计、`pet` 宠物动作与状态、`memory` 长期记忆历史与消歧）。找到工具后通过 `use_tool` 调用。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索词，如 `fridge 菜单 待办`、`album 照片 统计`、`pet`、`memory`" },
                "limit": { "type": "integer", "default": 6 }
            },
            "required": ["query"]
        })
    }
    fn run<'a>(&'a self, _ctx: &'a ToolExecContext, _args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move { Err("handled by ToolRegistry::execute_search_tool".into()) })
    }
}

struct UseToolMeta;
impl AgentTool for UseToolMeta {
    fn name(&self) -> &'static str {
        "use_tool"
    }
    fn domain(&self) -> &'static str {
        "meta"
    }
    fn always_loaded(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "调用通过 `search_tool` 发现的任何平台专属工具或 MCP 工具。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tool_name": { "type": "string", "description": "目标工具名，如 `fridge__get_overview` 或 `fridge__pin_today_menu`" },
                "arguments": { "type": "object", "description": "符合该工具 input_schema 的参数对象" }
            },
            "required": ["tool_name"]
        })
    }
    fn run<'a>(&'a self, _ctx: &'a ToolExecContext, _args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move { Err("handled by ToolRegistry::dispatch".into()) })
    }
}

// ============================================================================
// 2. KeepDays Platform Domain Tools (`fridge__*`, `album__*`, `pet__*`, `memory__*`)
// ============================================================================

struct FridgeOverviewTool;
impl AgentTool for FridgeOverviewTool {
    fn name(&self) -> &'static str {
        "fridge__get_overview"
    }
    fn domain(&self) -> &'static str {
        "fridge"
    }
    fn description(&self) -> &'static str {
        "读取当前空间冰箱门上的最近今日菜单便签、留言板字条（自动过滤未解锁时光胶囊）、未完成与已归档待办事项、以及全部家庭菜谱库。"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, _args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let now = now_ms();
            let f_doc: FridgeDoc = read_space_module(&ctx.state, ctx.space_id, dim::MOD_FRIDGE).await;
            let m_doc: MenuDoc = read_space_module(&ctx.state, ctx.space_id, dim::MOD_MENU).await;
            let mut b_doc: BoardDoc = read_space_module(&ctx.state, ctx.space_id, dim::MOD_BOARD).await;

            // Strictly redact time-capsule locked notes!
            for n in &mut b_doc.notes {
                if n.unlock_at.is_some_and(|u| u > now) {
                    n.locked = true;
                    n.text = "(时光胶囊未到开启时刻)".to_string();
                }
            }
            let active_todos: Vec<&BoardTodo> = b_doc.todos.iter().filter(|t| !t.done).collect();
            let archived_count = b_doc.todos.iter().filter(|t| t.done).count();

            let out = json!({
                "recent_menu_slips": f_doc.recent_slips,
                "menu_dishes": m_doc.dishes,
                "board_notes": b_doc.notes,
                "active_todos": active_todos,
                "archived_todos_count": archived_count,
            });
            Ok(ToolExecOutput {
                display_summary: "已读取冰箱便签、待办与家庭菜谱".to_string(),
                model_text: serde_json::to_string_pretty(&out).unwrap_or_default(),
                emitted_surface: None,
            })
        })
    }
}

struct FridgePostNoteTool;
impl AgentTool for FridgePostNoteTool {
    fn name(&self) -> &'static str {
        "fridge__post_note"
    }
    fn domain(&self) -> &'static str {
        "fridge"
    }
    fn description(&self) -> &'static str {
        "往冰箱留言板上贴一张便签字条，实时同步到双人画布。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "便签正文（1~280字）" },
                "color": { "type": "string", "enum": ["oat", "cotton_white", "sage", "blush"], "default": "oat" },
                "pinned": { "type": "boolean", "default": false }
            },
            "required": ["text"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "便签内容不能为空".to_string())?;
            let color = args.get("color").and_then(|v| v.as_str()).unwrap_or("oat");
            let pinned = args.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false);

            let note = BoardNote {
                id: ctx.state.snowflake.next_id(),
                uid: ctx.user_id,
                text: text.chars().take(280).collect(),
                color: color.to_string(),
                pinned,
                unlock_at: None,
                locked: false,
                at: now_ms(),
            };
            let note_clone = note.clone();
            mutate_space_module::<BoardDoc, _, _>(
                &ctx.state,
                ctx.space_id,
                dim::MOD_BOARD,
                move |board| {
                    board.notes.insert(0, note_clone);
                    board.notes.truncate(50);
                    Ok(())
                },
            )
            .await?;

            ctx.state
                .hub
                .publish(ctx.space_id, "BOARD_UPDATED", json!({ "by_agent": true }))
                .await;

            Ok(ToolExecOutput {
                display_summary: "已往冰箱贴上新字条".to_string(),
                model_text: format!("已贴上冰箱便签：「{}」", note.text),
                emitted_surface: None,
            })
        })
    }
}

struct FridgeManageTodoTool;
impl AgentTool for FridgeManageTodoTool {
    fn name(&self) -> &'static str {
        "fridge__manage_todo"
    }
    fn domain(&self) -> &'static str {
        "fridge"
    }
    fn description(&self) -> &'static str {
        "管理冰箱待办清单：支持新增待办 (`add`)、完成并自动归档 (`complete`)、恢复已归档待办 (`restore`)。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["add", "complete", "restore"] },
                "text": { "type": "string", "description": "新增待办文本（或按关键词匹配要完成/恢复的待办）" },
                "todo_id": { "type": "string", "description": "可选：精确待办 ID" },
                "category": { "type": "string", "enum": ["日常", "采购", "出行", "纪念"], "default": "日常" }
            },
            "required": ["action"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("add").to_string();
            let now = now_ms();
            let new_todo_id = ctx.state.snowflake.next_id();
            let actor_uid = ctx.user_id;

            let msg = mutate_space_module::<BoardDoc, _, _>(
                &ctx.state,
                ctx.space_id,
                dim::MOD_BOARD,
                move |board| match action.as_str() {
                    "add" => {
                        let txt = args
                            .get("text")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .ok_or_else(|| "新增待办需要 `text`".to_string())?;
                        let cat = args
                            .get("category")
                            .and_then(|v| v.as_str())
                            .unwrap_or("日常")
                            .to_string();
                        board.todos.insert(
                            0,
                            BoardTodo {
                                id: new_todo_id,
                                text: txt.chars().take(140).collect(),
                                category: Some(cat),
                                done: false,
                                done_by: None,
                                done_at: None,
                                at: now,
                            },
                        );
                        Ok(format!("已添加待办：「{txt}」"))
                    }
                    "complete" | "restore" => {
                        let want_done = action == "complete";
                        let tid = args.get("todo_id").and_then(|v| v.as_str());
                        let kw = args.get("text").and_then(|v| v.as_str()).map(str::trim);
                        let target = board
                            .todos
                            .iter_mut()
                            .find(|t| {
                                if let Some(id_str) = tid {
                                    t.id.to_string() == id_str
                                } else if let Some(k) = kw {
                                    t.done != want_done && t.text.contains(k)
                                } else {
                                    false
                                }
                            })
                            .ok_or_else(|| "未找到匹配的待办事项".to_string())?;
                        target.done = want_done;
                        target.done_by = if want_done { Some(actor_uid) } else { None };
                        target.done_at = if want_done { Some(now) } else { None };
                        Ok(format!(
                            "已将待办「{}」{}",
                            target.text,
                            if want_done { "标记完成并归档" } else { "从归档恢复" }
                        ))
                    }
                    _ => Err("不支持的待办操作".to_string()),
                },
            )
            .await?;

            ctx.state
                .hub
                .publish(ctx.space_id, "BOARD_UPDATED", json!({ "by_agent": true }))
                .await;

            Ok(ToolExecOutput {
                display_summary: msg.clone(),
                model_text: msg,
                emitted_surface: None,
            })
        })
    }
}

struct FridgePinMenuTool;
impl AgentTool for FridgePinMenuTool {
    fn name(&self) -> &'static str {
        "fridge__pin_today_menu"
    }
    fn domain(&self) -> &'static str {
        "fridge"
    }
    fn description(&self) -> &'static str {
        "将选好的菜名列表与备注贴成冰箱门上的「今日菜单便签」（如果某道菜还不在家庭菜谱库里，会自动补充收录）。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "dishes": { "type": "array", "items": { "type": "string" }, "description": "菜名数组，如 [\"可乐鸡翅\", \"番茄滑蛋\"]" },
                "note": { "type": "string", "default": "" }
            },
            "required": ["dishes"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let dish_names: Vec<String> = args
                .get("dishes")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if dish_names.is_empty() {
                return Err("请至少提供 1 道菜名".to_string());
            }
            let note = args
                .get("note")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .chars()
                .take(120)
                .collect::<String>();

            let mut tx = ctx.state.db.write.begin().await.map_err(|e| e.to_string())?;
            let mut menu_doc: MenuDoc =
                read_space_module_in_tx(&mut tx, ctx.space_id, dim::MOD_MENU).await;
            let mut dish_ids = Vec::new();
            let mut menu_dirty = false;

            for dname in &dish_names {
                if let Some(existing) = menu_doc.dishes.iter().find(|d| d.name == *dname) {
                    dish_ids.push(existing.id);
                } else {
                    let nid = ctx.state.snowflake.next_id();
                    menu_doc.dishes.push(MenuDish {
                        id: nid,
                        name: dname.clone(),
                        category: "荤菜".to_string(),
                        cover_asset_id: None,
                        ingredients: Vec::new(),
                        note: String::new(),
                    });
                    dish_ids.push(nid);
                    menu_dirty = true;
                }
            }
            if menu_dirty {
                write_space_module_in_tx(&mut tx, ctx.space_id, dim::MOD_MENU, &menu_doc).await?;
            }

            let mut fridge_doc: FridgeDoc =
                read_space_module_in_tx(&mut tx, ctx.space_id, dim::MOD_FRIDGE).await;
            let bj = now_beijing();
            let slip = FridgeSlip {
                id: ctx.state.snowflake.next_id(),
                date_label: bj.format("%m.%d").to_string(),
                by_uid: ctx.user_id,
                dish_ids,
                dishes: dish_names.clone(),
                note,
                grocery_synced: false,
                done: false,
                at: now_ms(),
            };
            fridge_doc.recent_slips.insert(0, slip);
            fridge_doc.recent_slips.truncate(5);
            write_space_module_in_tx(&mut tx, ctx.space_id, dim::MOD_FRIDGE, &fridge_doc).await?;
            tx.commit().await.map_err(|e| e.to_string())?;

            ctx.state
                .hub
                .publish(ctx.space_id, "FRIDGE_UPDATED", json!({ "by_agent": true }))
                .await;

            let summary = format!("已贴上今日菜单：{}", dish_names.join(" · "));
            Ok(ToolExecOutput {
                display_summary: summary.clone(),
                model_text: summary,
                emitted_surface: None,
            })
        })
    }
}

struct FridgeUpsertDishTool;
impl AgentTool for FridgeUpsertDishTool {
    fn name(&self) -> &'static str {
        "fridge__upsert_dish"
    }
    fn domain(&self) -> &'static str {
        "fridge"
    }
    fn description(&self) -> &'static str {
        "向家庭菜单库新增或更新一道私房菜谱（分类：荤菜 / 素菜 / 汤羹 / 主食 / 甜饮）。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "category": { "type": "string", "enum": ["荤菜", "素菜", "汤羹", "主食", "甜饮"], "default": "荤菜" },
                "ingredients": { "type": "array", "items": { "type": "string" } },
                "note": { "type": "string" }
            },
            "required": ["name"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "菜名不能为空".to_string())?;
            let cat = args
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("荤菜")
                .to_string();
            let ing: Vec<String> = args
                .get("ingredients")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|i| i.as_str().map(|s| s.trim().to_string()))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let note = args.get("note").and_then(|v| v.as_str()).unwrap_or("").to_string();

            let name_owned = name.to_string();
            let new_dish_id = ctx.state.snowflake.next_id();
            mutate_space_module::<MenuDoc, _, _>(
                &ctx.state,
                ctx.space_id,
                dim::MOD_MENU,
                move |menu_doc| {
                    if let Some(ex) = menu_doc.dishes.iter_mut().find(|d| d.name == name_owned) {
                        ex.category = cat;
                        if !ing.is_empty() {
                            ex.ingredients = ing;
                        }
                        if !note.is_empty() {
                            ex.note = note;
                        }
                    } else {
                        menu_doc.dishes.push(MenuDish {
                            id: new_dish_id,
                            name: name_owned,
                            category: cat,
                            cover_asset_id: None,
                            ingredients: ing,
                            note,
                        });
                    }
                    Ok(())
                },
            )
            .await?;
            ctx.state
                .hub
                .publish(ctx.space_id, "FRIDGE_UPDATED", json!({ "by_agent": true }))
                .await;

            Ok(ToolExecOutput {
                display_summary: format!("已收录家庭菜谱「{name}」"),
                model_text: format!("成功收录/更新菜谱「{name}」"),
                emitted_surface: None,
            })
        })
    }
}

struct AlbumSearchPhotosTool;
impl AgentTool for AlbumSearchPhotosTool {
    fn name(&self) -> &'static str {
        "album__search_photos"
    }
    fn domain(&self) -> &'static str {
        "album"
    }
    fn description(&self) -> &'static str {
        "检索当前空间照片墙回忆（可按配文/地点关键词或年月过滤），返回 photo_id、缩略图 URL 与拍摄日期，可直接填入 A2UI 的 `PhotoItem` 组件供主人点击看大图。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "keyword": { "type": "string", "description": "可选：匹配配文或拍摄地点的关键词" },
                "year_month": { "type": "string", "description": "可选：如 `2026-11` 或 `2025`" },
                "limit": { "type": "integer", "default": 8 }
            }
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let kw = args
                .get("keyword")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty());
            let ym = args
                .get("year_month")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(8).clamp(1, 20) as usize;

            let rows = sqlx::query(
                "SELECT id, asset_id, taken_at, post_json FROM photos
                 WHERE space_id = ?1
                 ORDER BY taken_at DESC, id DESC
                 LIMIT 200",
            )
            .bind(ctx.space_id)
            .fetch_all(&ctx.state.db.read)
            .await
            .map_err(|e| e.to_string())?;

            let mut matched = Vec::new();
            let mut asset_ids = Vec::new();
            for r in rows {
                let pid: i64 = r.get("id");
                let aid: i64 = r.get("asset_id");
                let taken_at: i64 = r.get("taken_at");
                let post: PhotoPostDoc = match serde_json::from_str(r.get("post_json")) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let dt_str = chrono::DateTime::from_timestamp_millis(taken_at)
                    .map(|d| d.with_timezone(&chrono_tz::Asia::Shanghai).format("%Y-%m-%d").to_string())
                    .unwrap_or_default();

                if let Some(ym_filter) = ym {
                    if !dt_str.starts_with(ym_filter) {
                        continue;
                    }
                }
                if let Some(ref k) = kw {
                    let loc = post.exif.location.clone().unwrap_or_default().to_lowercase();
                    if !post.caption.to_lowercase().contains(k) && !loc.contains(k) {
                        continue;
                    }
                }
                asset_ids.push(aid);
                matched.push((pid, aid, dt_str, post));
                if matched.len() >= limit {
                    break;
                }
            }

            let urls = resolve_asset_urls(&ctx.state, &asset_ids)
                .await
                .unwrap_or_default();

            let items: Vec<Value> = matched
                .into_iter()
                .map(|(pid, aid, date, post)| {
                    let u = urls.get(&aid.to_string());
                    json!({
                        "photo_id": pid.to_string(),
                        "date": date,
                        "caption": post.caption,
                        "location": post.exif.location,
                        "likes_count": post.likes.len(),
                        "comments_count": post.comments.len(),
                        "thumb_url": u.and_then(|x| x.thumb_url.clone()).or_else(|| u.map(|x| x.full_url.clone())),
                    })
                })
                .collect();

            Ok(ToolExecOutput {
                display_summary: format!("在相册中找到 {} 张照片回忆", items.len()),
                model_text: serde_json::to_string_pretty(&json!({ "photos": items })).unwrap_or_default(),
                emitted_surface: None,
            })
        })
    }
}

struct AlbumStatsSummaryTool;
impl AgentTool for AlbumStatsSummaryTool {
    fn name(&self) -> &'static str {
        "album__stats_summary"
    }
    fn domain(&self) -> &'static str {
        "album"
    }
    fn description(&self) -> &'static str {
        "汇总当前空间的月度照片数、留言互动数、菜谱分类占比、待办完成率与每日活跃热力数据，方便直接用 `a2ui_surface` 绘制 `Chart` 或 `Heatmap` 数据图表。"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, _args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT taken_at, post_json FROM photos WHERE space_id = ?1 ORDER BY taken_at DESC LIMIT 500",
            )
            .bind(ctx.space_id)
            .fetch_all(&ctx.state.db.read)
            .await
            .unwrap_or_default();

            let mut by_month: HashMap<String, usize> = HashMap::new();
            let mut by_day: HashMap<String, usize> = HashMap::new();
            let mut total_comments = 0usize;
            for r in &rows {
                let ts: i64 = r.get("taken_at");
                if let Some(dt) = chrono::DateTime::from_timestamp_millis(ts) {
                    let bj = dt.with_timezone(&chrono_tz::Asia::Shanghai);
                    *by_month.entry(bj.format("%Y-%m").to_string()).or_insert(0) += 1;
                    *by_day.entry(bj.format("%Y-%m-%d").to_string()).or_insert(0) += 1;
                }
                if let Ok(p) = serde_json::from_str::<PhotoPostDoc>(r.get("post_json")) {
                    total_comments += p.comments.len();
                }
            }

            let menu_doc: MenuDoc =
                read_space_module(&ctx.state, ctx.space_id, dim::MOD_MENU).await;
            let mut dish_by_cat: HashMap<String, usize> = HashMap::new();
            for d in &menu_doc.dishes {
                *dish_by_cat.entry(d.category.clone()).or_insert(0) += 1;
            }

            let board_doc: BoardDoc =
                read_space_module(&ctx.state, ctx.space_id, dim::MOD_BOARD).await;
            let done_todos = board_doc.todos.iter().filter(|t| t.done).count();
            let open_todos = board_doc.todos.iter().filter(|t| !t.done).count();

            let mut monthly_vec: Vec<(String, usize)> = by_month.into_iter().collect();
            monthly_vec.sort_by(|a, b| a.0.cmp(&b.0));

            let out = json!({
                "total_photos": rows.len(),
                "total_photo_comments": total_comments,
                "photos_by_month": monthly_vec.into_iter().map(|(k, v)| json!({"label": k, "value": v})).collect::<Vec<_>>(),
                "daily_heatmap": by_day,
                "dishes_by_category": dish_by_cat.into_iter().map(|(k, v)| json!({"label": k, "value": v})).collect::<Vec<_>>(),
                "todos": { "completed_archived": done_todos, "open": open_todos },
                "notes_count": board_doc.notes.len(),
            });

            Ok(ToolExecOutput {
                display_summary: "已汇总小家相册与生活统计数据".to_string(),
                model_text: serde_json::to_string_pretty(&out).unwrap_or_default(),
                emitted_surface: None,
            })
        })
    }
}

struct PetActAndEmoteTool;
impl AgentTool for PetActAndEmoteTool {
    fn name(&self) -> &'static str {
        "pet__act_and_emote"
    }
    fn domain(&self) -> &'static str {
        "pet"
    }
    fn description(&self) -> &'static str {
        "控制自己在主画布上的实时姿态（伸懒腰、梳理毛发、偷看、慢步、趴睡）或在头顶冒出一句纸质轻语气泡。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pose": {
                    "type": "string",
                    "enum": ["sitting", "stretching", "grooming", "peeking", "walking", "sleeping"]
                },
                "bubble_text": { "type": "string", "description": "可选：画布宠物头顶浮现的简短轻语（<= 36 字）" }
            },
            "required": ["pose"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let pose = args.get("pose").and_then(|v| v.as_str()).unwrap_or("sitting");
            let bubble = args
                .get("bubble_text")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().chars().take(36).collect::<String>())
                .filter(|s| !s.is_empty());

            if let Ok(mut tx) = ctx.state.db.write.begin().await {
                if let Ok(Some(row)) = sqlx::query(
                    "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
                )
                .bind(ctx.space_id)
                .bind(dim::MOD_PET)
                .fetch_optional(&mut *tx)
                .await
                {
                    if let Ok(mut pet) = serde_json::from_str::<PetStateDoc>(row.get("data_json")) {
                        if pet.adopted {
                            pet.stats.mood = (pet.stats.mood + 2).min(100);
                            if let Some(ref b) = bubble {
                                pet.push_log(PetLogEntry {
                                    uid: ctx.user_id,
                                    action: "AGENT_WHISPER".to_string(),
                                    text: format!("轻声说：「{b}」"),
                                    resonance: false,
                                    at: now_ms(),
                                });
                            }
                            if write_space_module_in_tx(&mut tx, ctx.space_id, dim::MOD_PET, &pet)
                                .await
                                .is_ok()
                            {
                                let _ = tx.commit().await;
                            }
                        }
                    }
                }
            }

            ctx.state
                .hub
                .publish(
                    ctx.space_id,
                    "PET_AGENT_EMOTE",
                    json!({
                        "pose": pose,
                        "bubble_text": bubble,
                        "for_user_id": ctx.user_id.to_string(),
                    }),
                )
                .await;

            Ok(ToolExecOutput {
                display_summary: format!("切换姿态为 {pose}"),
                model_text: "已在画布上同步动作与气泡（提示：请继续在对话正文里直接回复主人）".to_string(),
                emitted_surface: None,
            })
        })
    }
}

struct PetGetVitalsTool;
impl AgentTool for PetGetVitalsTool {
    fn name(&self) -> &'static str {
        "pet__get_vitals"
    }
    fn domain(&self) -> &'static str {
        "pet"
    }
    fn description(&self) -> &'static str {
        "读取宠物自身的等级、饱食度、精力、心情、羁绊值、性格倾向及小家成立天数。"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, _args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let pet: PetStateDoc =
                read_space_module(&ctx.state, ctx.space_id, dim::MOD_PET).await;
            let space_created: i64 = sqlx::query_scalar("SELECT created_at FROM spaces WHERE id = ?1")
                .bind(ctx.space_id)
                .fetch_optional(&ctx.state.db.read)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(now_ms);
            let days = ((now_ms() - space_created).max(0) / 86_400_000) + 1;

            let out = json!({
                "name": pet.name,
                "species": pet.species,
                "level": pet.level,
                "paw_coins": pet.paw_coins,
                "stats": pet.stats,
                "traits": pet.traits,
                "space_together_days": days,
            });
            Ok(ToolExecOutput {
                display_summary: "已读取自身状态与羁绊天数".to_string(),
                model_text: serde_json::to_string_pretty(&out).unwrap_or_default(),
                emitted_surface: None,
            })
        })
    }
}

struct MemoryCommitHistoryTool;
impl AgentTool for MemoryCommitHistoryTool {
    fn name(&self) -> &'static str {
        "memory__commit_history"
    }
    fn domain(&self) -> &'static str {
        "memory"
    }
    fn description(&self) -> &'static str {
        "查询长期记忆的 Git 式变更历史（仅限 `shared` 公共域与当前对话人自己的私密域），用于回忆习惯演进轨迹。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "keyword": { "type": "string", "description": "可选：按槽位键名或内容过滤" }
            }
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            let kw = args
                .get("keyword")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty());
            let u_scope = user_scope(ctx.user_id);

            let rows = sqlx::query(
                "SELECT rev_id, scope, parent_rev, source_session_id, message, ops_json, created_at
                 FROM agent_ltm_commits
                 WHERE space_id = ?1 AND scope IN ('shared', ?2)
                 ORDER BY created_at DESC LIMIT 20",
            )
            .bind(ctx.space_id)
            .bind(&u_scope)
            .fetch_all(&ctx.state.db.read)
            .await
            .map_err(|e| e.to_string())?;

            let mut list = Vec::new();
            for r in rows {
                let msg: String = r.get("message");
                let ops_raw: String = r.get("ops_json");
                if let Some(ref k) = kw {
                    if !msg.to_lowercase().contains(k) && !ops_raw.to_lowercase().contains(k) {
                        continue;
                    }
                }
                list.push(json!({
                    "rev_id": r.get::<String, _>("rev_id"),
                    "scope": r.get::<String, _>("scope"),
                    "parent_rev": r.get::<Option<String>, _>("parent_rev"),
                    "message": msg,
                    "ops": serde_json::from_str::<Value>(&ops_raw).unwrap_or(json!([])),
                    "created_at": r.get::<i64, _>("created_at"),
                }));
            }

            Ok(ToolExecOutput {
                display_summary: format!("查阅了 {} 条长期记忆变更记录", list.len()),
                model_text: serde_json::to_string_pretty(&json!({ "commits": list }))
                    .unwrap_or_default(),
                emitted_surface: None,
            })
        })
    }
}

struct MemoryResolveClarificationTool;
impl AgentTool for MemoryResolveClarificationTool {
    fn name(&self) -> &'static str {
        "memory__resolve_clarification"
    }
    fn domain(&self) -> &'static str {
        "memory"
    }
    fn description(&self) -> &'static str {
        "当主人在对话中澄清了之前的模糊记忆或确认了某条长期偏好的变动时，消除待追问项并提交一条原子 Git 式长期记忆变更。"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "clarification_id": { "type": "string", "description": "可选：待追问项 ID" },
                "scope": { "type": "string", "enum": ["private", "shared"], "default": "private" },
                "slot_key": { "type": "string", "description": "如 `place.favorite_dinner` 或 `diet.spicy`" },
                "new_val": { "type": "string", "description": "确认后的清晰记忆值" },
                "reason": { "type": "string", "default": "主人对话确认" }
            },
            "required": ["slot_key", "new_val"]
        })
    }
    fn run<'a>(&'a self, ctx: &'a ToolExecContext, args: Value) -> BoxFuture<'a, Result<ToolExecOutput, String>> {
        Box::pin(async move {
            if let Some(cid_str) = args.get("clarification_id").and_then(|v| v.as_str()) {
                if let Ok(cid) = cid_str.parse::<i64>() {
                    let _ = sqlx::query(
                        "UPDATE agent_clarifications SET status = 'RESOLVED', resolved_at = ?1
                         WHERE id = ?2 AND space_id = ?3 AND user_id = ?4",
                    )
                    .bind(now_ms())
                    .bind(cid)
                    .bind(ctx.space_id)
                    .bind(ctx.user_id)
                    .execute(&ctx.state.db.write)
                    .await;
                }
            }

            let target_scope = match args.get("scope").and_then(|v| v.as_str()) {
                Some("shared") => "shared".to_string(),
                _ => user_scope(ctx.user_id),
            };
            let slot_key = args
                .get("slot_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let new_val = args
                .get("new_val")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let reason = args
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("主人对话确认")
                .to_string();

            let commit = apply_ltm_git_commit(
                &ctx.state,
                ctx.space_id,
                &target_scope,
                &ctx.session_id,
                &reason,
                vec![LtmDiffOp {
                    op: "REPLACE".to_string(),
                    slot_key: slot_key.clone(),
                    old_val: None,
                    new_val: Some(new_val.clone()),
                    reason: reason.clone(),
                }],
            )
            .await?;

            Ok(ToolExecOutput {
                display_summary: format!("已更新长期记忆槽位 `{slot_key}`"),
                model_text: serde_json::to_string_pretty(&json!({ "committed": commit }))
                    .unwrap_or_default(),
                emitted_surface: None,
            })
        })
    }
}

async fn try_read_space_module<T: serde::de::DeserializeOwned>(
    state: &AppState,
    space_id: i64,
    module_type_id: i64,
) -> Result<T, String> {
    let row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(space_id)
    .bind(module_type_id)
    .fetch_optional(&state.db.read)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "模块不存在".to_string())?;
    serde_json::from_str(row.get("data_json")).map_err(|e| e.to_string())
}

async fn read_space_module<T: serde::de::DeserializeOwned + Default>(
    state: &AppState,
    space_id: i64,
    module_type_id: i64,
) -> T {
    try_read_space_module(state, space_id, module_type_id)
        .await
        .unwrap_or_default()
}

async fn read_space_module_in_tx<T: serde::de::DeserializeOwned + Default>(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    space_id: i64,
    module_type_id: i64,
) -> T {
    let Ok(Some(row)) = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(space_id)
    .bind(module_type_id)
    .fetch_optional(&mut **tx)
    .await
    else {
        return T::default();
    };
    serde_json::from_str(row.get("data_json")).unwrap_or_default()
}

async fn write_space_module_in_tx<T: serde::Serialize>(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    space_id: i64,
    module_type_id: i64,
    doc: &T,
) -> Result<(), String> {
    let raw = serde_json::to_string(doc).map_err(|e| e.to_string())?;
    sqlx::query(
        "UPDATE space_modules
         SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4",
    )
    .bind(raw)
    .bind(now_ms())
    .bind(space_id)
    .bind(module_type_id)
    .execute(&mut **tx)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn mutate_space_module<T, R, F>(
    state: &AppState,
    space_id: i64,
    module_type_id: i64,
    mutator: F,
) -> Result<R, String>
where
    T: serde::de::DeserializeOwned + serde::Serialize + Default,
    F: FnOnce(&mut T) -> Result<R, String>,
{
    let mut tx = state.db.write.begin().await.map_err(|e| e.to_string())?;
    let mut doc: T = read_space_module_in_tx(&mut tx, space_id, module_type_id).await;
    let out = mutator(&mut doc)?;
    write_space_module_in_tx(&mut tx, space_id, module_type_id, &doc).await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(out)
}
