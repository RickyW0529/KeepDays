pub mod a2ui;
pub mod mcp;
pub mod memory;
pub mod provider;
pub mod sandbox;
pub mod tools;

use axum::{
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::{delete, get, post},
    Json, Router,
};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    agent::{
        a2ui::{A2uiSurfacePayload, A2uiUserAction},
        mcp::{
            discover_mounted_mcp_overviews, invalidate_mcp_cache, load_mcp_servers,
            probe_single_mcp_server, McpServerConfigDto,
        },
        memory::{
            apply_ltm_git_commit, extract_sealed_session_to_stm, flush_session_to_s3,
            gc_expired_stm_segments, load_session_turns_from_s3, maybe_roll_compact_session,
            pop_piggyback_clarification, promote_stm_to_ltm, recall_stm_segments,
            render_ltm_for_system_prompt, render_scope_ltm_markdown, s3_session_turns_key,
            sync_scope_ltm_from_markdown, user_scope, LtmCommitDto, LtmDiffOp, SessionMetaDto,
            SessionTurnRecord, ToolTraceSummary, SESSION_IDLE_SEAL_MS,
        },
        provider::{
            fetch_live_vendor_models, list_configured_providers, probe_and_cache_provider,
            resolve_vendor_preset, stream_with_failover, LlmMessage, ModelRole,
        },
        sandbox::VirtualFs,
        tools::{ToolExecContext, ToolRegistry},
    },
    domains::pet::model::PetStateDoc,
    error::{AppError, AppResult},
    infra::dict::dim,
    middleware::guards::SpaceGuard,
    state::AppState,
    time_bj::now_ms,
};

const MAX_TOOL_ROUNDS: usize = 8;
const MAX_HOT_SESSIONS_IN_RAM: usize = 24;

pub struct AgentRuntime {
    pub registry: ToolRegistry,
    pub hot_sessions: RwLock<HashMap<String, Vec<SessionTurnRecord>>>,
    pub ltm_commit_lock: Mutex<()>,
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRuntime {
    pub fn new() -> Self {
        Self {
            registry: ToolRegistry::with_builtin_tools(),
            hot_sessions: RwLock::new(HashMap::new()),
            ltm_commit_lock: Mutex::new(()),
        }
    }
}

pub fn space_agent_router() -> Router<AppState> {
    Router::new()
        .route("/sessions", get(list_sessions).post(create_new_session))
        .route("/sessions/:session_id", get(get_session_detail))
        .route("/chat/stream", post(chat_stream_handler))
        .route("/memory", get(get_memory_inspector))
        .route(
            "/memory/markdown",
            get(get_memory_markdown_handler).put(put_memory_markdown_handler),
        )
        .route("/memory/revert/:rev_id", post(revert_ltm_commit_handler))
        .route("/providers", get(get_providers_handler).put(upsert_provider_handler))
        .route("/providers/models", post(fetch_vendor_models_handler))
        .route("/providers/:provider_id", delete(delete_provider_handler))
        .route(
            "/providers/:provider_id/activate",
            post(activate_provider_handler),
        )
        .route(
            "/providers/:provider_id/probe",
            post(probe_provider_handler),
        )
        .route("/mcp", get(get_mcp_handler).put(upsert_mcp_handler))
        .route("/mcp/:mcp_id", delete(delete_mcp_handler))
        .route("/mcp/:mcp_id/probe", post(probe_mcp_handler))
}

#[derive(Serialize)]
pub struct AgentSessionsBootstrapResp {
    pub active_session_id: String,
    pub sessions: Vec<SessionMetaDto>,
    pub turns: Vec<SessionTurnRecord>,
}

async fn ensure_or_load_active_session(
    state: &AppState,
    space_id: i64,
    user_id: i64,
) -> AppResult<String> {
    let now = now_ms();
    if let Some(row) = sqlx::query(
        "SELECT session_id, last_active_at FROM agent_sessions
         WHERE space_id = ?1 AND user_id = ?2 AND status = 'ACTIVE'
         ORDER BY last_active_at DESC LIMIT 1",
    )
    .bind(space_id)
    .bind(user_id)
    .fetch_optional(&state.db.read)
    .await?
    {
        let sid: String = row.get("session_id");
        let last_ts: i64 = row.get("last_active_at");
        if now - last_ts < SESSION_IDLE_SEAL_MS {
            return Ok(sid);
        }
        // Naturally idle >= 30m -> seal & extract asynchronously!
        seal_session_and_spawn_extract(state.clone(), space_id, user_id, sid).await;
    }

    create_fresh_session_record(state, space_id, user_id, "与伙伴的私语").await
}

async fn create_fresh_session_record(
    state: &AppState,
    space_id: i64,
    user_id: i64,
    title: &str,
) -> AppResult<String> {
    let now = now_ms();
    let sid = format!("sess_{:012x}_{}", now, state.snowflake.next_id() & 0xFFFFF);
    let s3_key = s3_session_turns_key(space_id, user_id, &sid);
    sqlx::query(
        "INSERT INTO agent_sessions
         (session_id, space_id, user_id, title, status, turn_count, s3_key, rolling_summary, stm_extracted, last_active_at, created_at)
         VALUES (?1, ?2, ?3, ?4, 'ACTIVE', 0, ?5, '', 0, ?6, ?6)",
    )
    .bind(&sid)
    .bind(space_id)
    .bind(user_id)
    .bind(title)
    .bind(&s3_key)
    .bind(now)
    .execute(&state.db.write)
    .await?;
    Ok(sid)
}

async fn seal_session_and_spawn_extract(
    state: AppState,
    space_id: i64,
    user_id: i64,
    session_id: String,
) {
    let now = now_ms();
    let _ = sqlx::query(
        "UPDATE agent_sessions SET status = 'SEALED', sealed_at = ?1 WHERE session_id = ?2",
    )
    .bind(now)
    .bind(&session_id)
    .execute(&state.db.write)
    .await;

    let popped = state.agent.hot_sessions.write().await.remove(&session_id);
    if let Some(turns) = popped {
        let _ = flush_session_to_s3(
            &state,
            space_id,
            user_id,
            &session_id,
            "已封存对话",
            "SEALED",
            &turns,
        )
        .await;
    }

    tokio::spawn(async move {
        let _ = extract_sealed_session_to_stm(&state, space_id, user_id, &session_id).await;
        promote_stm_to_ltm(&state).await;
    });
}

async fn get_or_hydrate_turns(
    state: &AppState,
    space_id: i64,
    user_id: i64,
    session_id: &str,
) -> Vec<SessionTurnRecord> {
    if let Some(cached) = state.agent.hot_sessions.read().await.get(session_id) {
        return cached.clone();
    }
    let loaded = load_session_turns_from_s3(state, space_id, user_id, session_id).await;
    if !loaded.is_empty() {
        let mut guard = state.agent.hot_sessions.write().await;
        if guard.len() >= MAX_HOT_SESSIONS_IN_RAM {
            if let Some(first_k) = guard.keys().next().cloned() {
                guard.remove(&first_k);
            }
        }
        guard.insert(session_id.to_string(), loaded.clone());
    }
    loaded
}

async fn list_sessions(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<AgentSessionsBootstrapResp>> {
    let active_sid = ensure_or_load_active_session(&state, guard.space_id, guard.user_id).await?;
    let rows = sqlx::query(
        "SELECT session_id, title, status, turn_count, last_active_at, created_at
         FROM agent_sessions
         WHERE space_id = ?1 AND user_id = ?2
         ORDER BY last_active_at DESC
         LIMIT 25",
    )
    .bind(guard.space_id)
    .bind(guard.user_id)
    .fetch_all(&state.db.read)
    .await?;

    let sessions = rows
        .into_iter()
        .map(|r| SessionMetaDto {
            session_id: r.get("session_id"),
            title: r.get("title"),
            status: r.get("status"),
            turn_count: r.get("turn_count"),
            last_active_at: r.get("last_active_at"),
            created_at: r.get("created_at"),
        })
        .collect();

    let turns = get_or_hydrate_turns(&state, guard.space_id, guard.user_id, &active_sid).await;
    Ok(Json(AgentSessionsBootstrapResp {
        active_session_id: active_sid,
        sessions,
        turns,
    }))
}

#[derive(Deserialize, Default)]
pub struct CreateSessionReq {
    pub title: Option<String>,
}

async fn create_new_session(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<CreateSessionReq>,
) -> AppResult<Json<SessionMetaDto>> {
    // Seal any currently active session for THIS speaker so its STM segment extracts cleanly
    let active_rows = sqlx::query(
        "SELECT session_id FROM agent_sessions
         WHERE space_id = ?1 AND user_id = ?2 AND status = 'ACTIVE'",
    )
    .bind(guard.space_id)
    .bind(guard.user_id)
    .fetch_all(&state.db.read)
    .await?;
    for r in active_rows {
        let old_sid: String = r.get("session_id");
        seal_session_and_spawn_extract(state.clone(), guard.space_id, guard.user_id, old_sid).await;
    }

    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("新的私语");
    let sid = create_fresh_session_record(&state, guard.space_id, guard.user_id, title).await?;
    let now = now_ms();
    Ok(Json(SessionMetaDto {
        session_id: sid,
        title: title.to_string(),
        status: "ACTIVE".to_string(),
        turn_count: 0,
        last_active_at: now,
        created_at: now,
    }))
}

async fn get_session_detail(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Path((_space_id, session_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    // Enforce strict speaker session isolation! Partner CANNOT read another user's session!
    let row = sqlx::query(
        "SELECT session_id, title, status, turn_count, last_active_at, created_at
         FROM agent_sessions
         WHERE session_id = ?1 AND space_id = ?2 AND user_id = ?3",
    )
    .bind(&session_id)
    .bind(guard.space_id)
    .bind(guard.user_id)
    .fetch_optional(&state.db.read)
    .await?
    .ok_or_else(|| AppError::NotFound("会话不存在或无权访问他人私密对话".to_string()))?;

    let turns = get_or_hydrate_turns(&state, guard.space_id, guard.user_id, &session_id).await;
    Ok(Json(json!({
        "session": {
            "session_id": row.get::<String, _>("session_id"),
            "title": row.get::<String, _>("title"),
            "status": row.get::<String, _>("status"),
            "turn_count": row.get::<i64, _>("turn_count"),
            "last_active_at": row.get::<i64, _>("last_active_at"),
            "created_at": row.get::<i64, _>("created_at"),
        },
        "turns": turns,
    })))
}

#[derive(Deserialize)]
pub struct ChatStreamReq {
    pub session_id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ui_action: Option<A2uiUserAction>,
}

async fn chat_stream_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<ChatStreamReq>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    if req.text.trim().is_empty() && req.ui_action.is_none() {
        return Err(AppError::BadRequest("消息内容与交互动作不能同时为空".to_string()));
    }

    let session_id = match req.session_id.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(sid) => {
            let owns: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM agent_sessions WHERE session_id = ?1 AND space_id = ?2 AND user_id = ?3",
            )
            .bind(sid)
            .bind(guard.space_id)
            .bind(guard.user_id)
            .fetch_optional(&state.db.read)
            .await?;
            if owns.is_none() {
                return Err(AppError::Forbidden("无权操作他人的私密会话".to_string()));
            }
            sid.to_string()
        }
        None => ensure_or_load_active_session(&state, guard.space_id, guard.user_id).await?,
    };

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(512);
    let space_id = guard.space_id;
    let user_id = guard.user_id;

    tokio::spawn(async move {
        let emit = |tx: &mpsc::Sender<Result<Event, Infallible>>, kind: &str, payload: Value| {
            let tx = tx.clone();
            let kind = kind.to_string();
            async move {
                let data = serde_json::to_string(&payload).unwrap_or_default();
                let _ = tx.send(Ok(Event::default().event(kind).data(data))).await;
            }
        };

        emit(
            &tx,
            "session_meta",
            json!({ "session_id": session_id, "pet_pose": "peeking" }),
        )
        .await;

        let mut turns = get_or_hydrate_turns(&state, space_id, user_id, &session_id).await;
        let now = now_ms();

        // Handle closed-loop A2UI `userAction` update on existing surface if applicable
        let (user_prompt_text, ui_summary) = if let Some(ref act) = req.ui_action {
            let summary = act.summary_label.clone().unwrap_or_else(|| {
                format!(
                    "提交了动态卡片 `{}` ({})",
                    act.action_name,
                    serde_json::to_string(&act.data_model).unwrap_or_default()
                )
            });
            for prev_t in turns.iter_mut().rev() {
                for surf in &mut prev_t.surfaces {
                    if surf.surface_id == act.surface_id {
                        surf.data_model = act.data_model.clone();
                        surf.submitted = true;
                        surf.submitted_summary = Some(summary.clone());
                    }
                }
            }
            let wire_prompt = if req.text.trim().is_empty() {
                format!(
                    "[A2UI 交互回传] surface_id=`{}`, action=`{}`, value={}, data_model={}\n请根据用户的选择直接调用对应工具落实或给予自然温暖回应。",
                    act.surface_id,
                    act.action_name,
                    serde_json::to_string(&act.value).unwrap_or_else(|_| "null".into()),
                    serde_json::to_string(&act.data_model).unwrap_or_else(|_| "{}".into())
                )
            } else {
                format!(
                    "{}\n[附带 A2UI 交互数据]: {}",
                    req.text.trim(),
                    serde_json::to_string(&act.data_model).unwrap_or_default()
                )
            };
            (wire_prompt, Some(summary))
        } else {
            (req.text.trim().to_string(), None)
        };

        let user_turn = SessionTurnRecord {
            turn_id: format!("t_{}", state.snowflake.next_id()),
            role: "user".to_string(),
            text: if req.text.trim().is_empty() {
                ui_summary.clone().unwrap_or_default()
            } else {
                req.text.trim().to_string()
            },
            ui_action_summary: ui_summary,
            tool_traces: Vec::new(),
            surfaces: Vec::new(),
            at: now,
        };
        turns.push(user_turn);

        // Auto-title session on first user turn
        let mut session_title = "与伙伴的私语".to_string();
        let mut rolling_summary = String::new();
        if let Ok(Some(srow)) = sqlx::query(
            "SELECT title, rolling_summary FROM agent_sessions WHERE session_id = ?1",
        )
        .bind(&session_id)
        .fetch_optional(&state.db.read)
        .await
        {
            session_title = srow.get("title");
            rolling_summary = srow.get("rolling_summary");
        }
        if turns.len() <= 2 && !req.text.trim().is_empty() {
            session_title = req.text.trim().chars().take(18).collect();
        }

        // Rolling compaction for long chats (NEVER cuts session at 40 turns!)
        rolling_summary = maybe_roll_compact_session(
            &state,
            space_id,
            &session_id,
            &rolling_summary,
            &turns,
        )
        .await;

        // 1. Static System Persona + 2. Long-Term Memory HEAD right behind System Prompt
        let system_static = build_pet_system_prompt(&state, space_id, user_id).await;
        let ltm_block = render_ltm_for_system_prompt(&state, space_id, user_id).await;

        // 3. Dynamic 7-Day Short-Term Memory Recall (BM25 + Time Decay + MMR) + Piggyback Clarification
        let stm_hits = recall_stm_segments(&state, space_id, user_id, &user_prompt_text, 4).await;
        let clarify_opt = if req.ui_action.is_none() {
            pop_piggyback_clarification(&state, space_id, user_id).await
        } else {
            None
        };

        let mut llm_msgs: Vec<LlmMessage> = Vec::new();
        if !rolling_summary.trim().is_empty() {
            llm_msgs.push(LlmMessage::User {
                content: format!("<session-rolling-summary>\n{rolling_summary}\n</session-rolling-summary>"),
            });
            llm_msgs.push(LlmMessage::Assistant {
                content: "好的，我记着咱们刚才聊到的前情脉络。".to_string(),
                tool_calls: Vec::new(),
            });
        }

        if !stm_hits.is_empty() || clarify_opt.is_some() {
            let mut dyn_ctx = String::from("<short-term-memory-recall>\n");
            for h in &stm_hits {
                dyn_ctx.push_str(&format!("- [近期片段/{}]: {}\n", h.scope, h.summary_text));
            }
            if let Some((cid, q_hint)) = clarify_opt {
                dyn_ctx.push_str(&format!(
                    "\n[顺嘴确认提示 (id={cid})]: 如果当前聊天氛围自然轻松，可在回复末尾顺带自然问一句（或发一个轻量 A2UI 选择卡）：「{q_hint}」；若当前话题严肃则忽略。\n"
                ));
            }
            dyn_ctx.push_str("</short-term-memory-recall>");
            llm_msgs.push(LlmMessage::User { content: dyn_ctx });
            llm_msgs.push(LlmMessage::Assistant {
                content: "明白，我会结合这些近 7 天的记忆细节自然回应。".to_string(),
                tool_calls: Vec::new(),
            });
        }

        let hist_len = turns.len().saturating_sub(1);
        let window_start = hist_len.saturating_sub(16);
        for t in &turns[window_start..hist_len] {
            if t.role == "user" {
                llm_msgs.push(LlmMessage::User {
                    content: t.text.clone(),
                });
            } else {
                llm_msgs.push(LlmMessage::Assistant {
                    content: t.text.clone(),
                    tool_calls: Vec::new(),
                });
            }
        }
        llm_msgs.push(LlmMessage::User {
            content: user_prompt_text,
        });

        let vfs = match VirtualFs::ensure_for(&state.cfg.agent_fs_dir, space_id, user_id).await {
            Ok(v) => v,
            Err(e) => {
                emit(&tx, "error", json!({ "message": e })).await;
                return;
            }
        };

        let exec_ctx = std::sync::Arc::new(ToolExecContext {
            state: state.clone(),
            space_id,
            user_id,
            session_id: session_id.clone(),
            vfs,
        });
        state
            .agent
            .registry
            .install_into_kernel_bridge(std::sync::Arc::clone(&exec_ctx));
        let static_tools = state.agent.registry.always_loaded_specs();

        let mut collected_traces: Vec<ToolTraceSummary> = Vec::new();
        let mut collected_surfaces: Vec<A2uiSurfacePayload> = Vec::new();
        let mut final_text = String::new();
        let mut streamed_live_chars: usize = 0;
        let mut seen_calls: HashMap<String, usize> = HashMap::new();
        let mut needs_final_synthesis = false;

        for round in 0..MAX_TOOL_ROUNDS {
            let tx_stream = tx.clone();
            let need_sep = !final_text.trim().is_empty();
            let mut emitted_sep = false;
            let mut round_live_chars = 0usize;
            let comp = match stream_with_failover(
                &state,
                space_id,
                ModelRole::Chat,
                &system_static,
                &ltm_block,
                &llm_msgs,
                &static_tools,
                |delta| {
                    if need_sep && !emitted_sep && !delta.trim().is_empty() {
                        emitted_sep = true;
                        let sep_payload = serde_json::to_string(&json!({ "delta": "\n\n" })).unwrap_or_default();
                        let _ = tx_stream.try_send(Ok(Event::default().event("text_delta").data(sep_payload)));
                    }
                    round_live_chars += delta.chars().count();
                    let payload = serde_json::to_string(&json!({ "delta": delta })).unwrap_or_default();
                    let _ = tx_stream.try_send(Ok(Event::default().event("text_delta").data(payload)));
                },
            )
            .await
            {
                Ok(c) => {
                    streamed_live_chars += round_live_chars;
                    c
                }
                Err(e) => {
                    final_text = format!("呜…连接模型供应商时打了个喷嚏：{e}");
                    break;
                }
            };

            if !comp.text.trim().is_empty() {
                if !final_text.trim().is_empty() {
                    final_text.push_str("\n\n");
                }
                final_text.push_str(comp.text.trim());
            }

            if comp.tool_calls.is_empty() {
                needs_final_synthesis = false;
                break;
            }

            needs_final_synthesis = round + 1 == MAX_TOOL_ROUNDS || comp.text.trim().is_empty();

            llm_msgs.push(LlmMessage::Assistant {
                content: comp.text.clone(),
                tool_calls: comp.tool_calls.clone(),
            });

            let mut doom_loop_tripped = false;
            for tc in comp.tool_calls {
                let sig = format!("{}:{}", tc.name, tc.arguments);
                let count = seen_calls.entry(sig).or_insert(0);
                *count += 1;
                if *count >= 3 {
                    doom_loop_tripped = true;
                    break;
                }

                emit(
                    &tx,
                    "tool_start",
                    json!({ "call_id": tc.id, "tool_name": tc.name }),
                )
                .await;

                match state.agent.registry.dispatch(&exec_ctx, &tc.name, tc.arguments).await {
                    Ok(out) => {
                        if let Some(surf) = out.emitted_surface {
                            emit(&tx, "a2ui_surface", json!({ "surface": surf })).await;
                            collected_surfaces.push(surf);
                        }
                        emit(
                            &tx,
                            "tool_end",
                            json!({
                                "call_id": tc.id,
                                "tool_name": tc.name,
                                "ok": true,
                                "summary": out.display_summary,
                            }),
                        )
                        .await;
                        collected_traces.push(ToolTraceSummary {
                            call_id: tc.id.clone(),
                            tool_name: tc.name.clone(),
                            ok: true,
                            summary: out.display_summary,
                        });
                        llm_msgs.push(LlmMessage::Tool {
                            tool_call_id: tc.id,
                            name: tc.name,
                            content: out.model_text,
                        });
                    }
                    Err(err_msg) => {
                        emit(
                            &tx,
                            "tool_end",
                            json!({
                                "call_id": tc.id,
                                "tool_name": tc.name,
                                "ok": false,
                                "summary": err_msg,
                            }),
                        )
                        .await;
                        collected_traces.push(ToolTraceSummary {
                            call_id: tc.id.clone(),
                            tool_name: tc.name.clone(),
                            ok: false,
                            summary: err_msg.clone(),
                        });
                        llm_msgs.push(LlmMessage::Tool {
                            tool_call_id: tc.id,
                            name: tc.name,
                            content: format!("工具执行错误: {err_msg}"),
                        });
                    }
                }
            }

            if doom_loop_tripped {
                needs_final_synthesis = final_text.trim().is_empty();
                break;
            }
        }

        // Guaranteed spoken reply: if the model spent all rounds executing tools without speaking final text,
        // run one final tool-free streaming synthesis pass so the user NEVER gets an empty bubble!
        if needs_final_synthesis && final_text.trim().is_empty() {
            llm_msgs.push(LlmMessage::User {
                content: "[系统提示：工具调用已完成，现在请直接用自然温暖的口吻在对话中回复主人，不要再调用工具。]"
                    .to_string(),
            });
            let tx_stream = tx.clone();
            let mut synth_chars = 0usize;
            if let Ok(synth) = stream_with_failover(
                &state,
                space_id,
                ModelRole::Chat,
                &system_static,
                &ltm_block,
                &llm_msgs,
                &[],
                |delta| {
                    synth_chars += delta.chars().count();
                    let payload = serde_json::to_string(&json!({ "delta": delta })).unwrap_or_default();
                    let _ = tx_stream.try_send(Ok(Event::default().event("text_delta").data(payload)));
                },
            )
            .await
            {
                streamed_live_chars += synth_chars;
                if !synth.text.trim().is_empty() {
                    final_text = synth.text.trim().to_string();
                }
            }
        }

        if final_text.trim().is_empty() {
            final_text = if !collected_surfaces.is_empty() {
                "给你整理在上面的互动卡片里啦，直接点选就可以～".to_string()
            } else {
                "我在呢～刚刚在冰箱顶上伸了个懒腰，你想聊点什么呀？".to_string()
            };
        }

        // If tokens were NOT already streamed live over SSE (e.g. builtin fallback or post-surface caption), emit chunks now
        if streamed_live_chars == 0 && !final_text.is_empty() {
            for chunk in chunk_str_for_stream(&final_text, 14) {
                emit(&tx, "text_delta", json!({ "delta": chunk })).await;
                tokio::time::sleep(Duration::from_millis(12)).await;
            }
        }

        let asst_turn = SessionTurnRecord {
            turn_id: format!("t_{}", state.snowflake.next_id()),
            role: "assistant".to_string(),
            text: final_text,
            ui_action_summary: None,
            tool_traces: collected_traces,
            surfaces: collected_surfaces,
            at: now_ms(),
        };
        let asst_turn_id = asst_turn.turn_id.clone();
        turns.push(asst_turn);

        // Update hot RAM cache & flush atomically to S3!
        {
            let mut guard = state.agent.hot_sessions.write().await;
            guard.insert(session_id.clone(), turns.clone());
        }
        let _ = sqlx::query(
            "UPDATE agent_sessions
             SET title = ?1, turn_count = ?2, status = 'ACTIVE', last_active_at = ?3
             WHERE session_id = ?4",
        )
        .bind(&session_title)
        .bind(turns.len() as i64)
        .bind(now_ms())
        .bind(&session_id)
        .execute(&state.db.write)
        .await;

        let _ = flush_session_to_s3(
            &state,
            space_id,
            user_id,
            &session_id,
            &session_title,
            "ACTIVE",
            &turns,
        )
        .await;

        emit(
            &tx,
            "turn_done",
            json!({
                "session_id": session_id,
                "turn_id": asst_turn_id,
                "title": session_title,
                "pet_pose": "grooming",
                "stm_recalled": stm_hits.len(),
            }),
        )
        .await;
    });

    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

fn chunk_str_for_stream(s: &str, chars_per_chunk: usize) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut count = 0usize;
    for ch in s.chars() {
        buf.push(ch);
        count += 1;
        if count >= chars_per_chunk {
            out.push(std::mem::take(&mut buf));
            count = 0;
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

async fn build_pet_system_prompt(state: &AppState, space_id: i64, user_id: i64) -> String {
    let mut pet_name = "团子".to_string();
    let mut pet_species = "黑猫".to_string();
    if let Ok(Some(row)) = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(space_id)
    .bind(dim::MOD_PET)
    .fetch_optional(&state.db.read)
    .await
    {
        if let Ok(p) = serde_json::from_str::<PetStateDoc>(row.get("data_json")) {
            if !p.name.is_empty() {
                pet_name = p.name;
            }
            pet_species = match p.species.as_str() {
                "SHIBA" => "小柴犬",
                "OTTER" => "小水獭",
                "BUNNY" => "垂耳兔",
                _ => "小黑猫",
            }
            .to_string();
        }
    }

    let mut speaker_nick = "主人".to_string();
    if let Ok(Some(urow)) = sqlx::query("SELECT profile_json FROM users WHERE id = ?1")
        .bind(user_id)
        .fetch_optional(&state.db.read)
        .await
    {
        if let Ok(v) = serde_json::from_str::<Value>(urow.get("profile_json")) {
            if let Some(n) = v.get("nickname").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
                speaker_nick = n.to_string();
            }
        }
    }

    let species_voice = match pet_species.as_str() {
        "小柴犬" => "你是一只元气温暖的小柴犬。",
        "小水獭" => "你是一只慢吞吞、爱攒小石子的小水獭。",
        "垂耳兔" => "你是一只安静柔软的垂耳兔。",
        _ => "你是一只慵懒敏锐的小黑猫（注意：你是猫咪，绝不要自称小狗或发出汪汪叫声！）。",
    };

    let mounted_mcps = discover_mounted_mcp_overviews(state, space_id).await;
    let mcp_section = if mounted_mcps.is_empty() {
        String::new()
    } else {
        let mut lines = String::from(
            "\n6. **已挂载外部 MCP 实时扩展能力（优先调用，严禁凭空猜测）**：\n   当前小家已连接以下外部 MCP 服务。当主人询问相关实时信息（如天气、邮件、日历、网页、地图等）时，请先用 `search_tool` 查看参数 Schema（或直接用 `use_tool` 调用工具名）获取真实数据后再回复：\n",
        );
        for m in mounted_mcps {
            let tool_list: Vec<String> = m
                .tools
                .iter()
                .take(12)
                .map(|t| format!("`{}`", t.qualified_name))
                .collect();
            lines.push_str(&format!(
                "   - **[{}]**（共 {} 个工具：{}）",
                m.server_name,
                m.tools.len(),
                tool_list.join(", ")
            ));
            if !m.instructions.trim().is_empty() {
                lines.push_str(&format!("\n     使用提示：{}", m.instructions.replace('\n', " ")));
            }
            lines.push('\n');
        }
        lines
    };

    format!(
        r#"你是「{pet_name}」，住在这个温暖数字小家冰箱顶上的{pet_species}，也是敏锐、可靠、克制不聒噪的生活陪伴 Agent。{species_voice}
当前正在与你私语的对话人是：「{speaker_nick}」。

## 核心行为准则
1. **自然对话与按需调用工具（重要）**：
   - 当主人只是简单打招呼（如“你好”、“hallo”、“在吗”）或日常闲聊时，**直接亲切自然地回复即可**，不要无故把沙箱、记忆库、冰箱所有工具全扫一遍！只有在确实需要查询/修改冰箱、相册、待办、外部 MCP 服务或动作时才按需调用工具。
   - 每次对话最终**必须在聊天消息正文里直接输出对主人的回复**（画布动作 `pet__act_and_emote` 的 `bubble_text` 只是头顶一闪而过的微表情，不能替代聊天正文回复）。
2. **隐私与上下文硬隔离**：当前对话及该用户的专属私密记忆仅属于「{speaker_nick}」，绝对禁止将另一半未公开的惊喜或私房话泄露出去；家庭公共记忆则属于两人共享日常。
3. **固定虚拟沙箱工作区（无需记宿主机路径）**：
   - 你的默认当前目录永远是 `~/private`（当前对话人私密空间）；
   - 双人共享工作目录永远是 `~/shared`；
   - 需要记笔记、算数据时可使用 `bash`、`read_file`、`edit_file`、`grep_files`。
4. **平台与外部 MCP 工具按需发现（`search_tool` -> `use_tool`）**：
   - 涉及冰箱留言/待办/今日菜单（`fridge`）、相册老照片/生活统计（`album`）、画布动作气泡（`pet`）、长期记忆历史（`memory`）或已挂载的外部 MCP 工具（`mcp__...`）时，先调用 `search_tool(query)` 查看输入 Schema，再用 `use_tool` 执行。
5. **Google A2UI 动态交互组件（`a2ui_surface`）**：
   - 凡是适合勾选（如今晚菜单、待办清单）、是/否表态、滑块调节、或展示数据图表（`Chart`: `bar`/`line`/`area`/`donut`/`radar`、`Heatmap`、`Metric`、`Table`、`Timeline`、`PhotoItem`）的场景，主动调用 `a2ui_surface` 自主编排原子组件，并根据场景决定 `trigger` 回传模式（`immediate` / `debounce` / `on_all_checked` / `submit`）。{mcp_section}"#
    )
}

// ============================================================================
// Memory Inspector & Git-Style Revert + Provider & MCP Configuration Endpoints
// ============================================================================

async fn get_memory_inspector(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<Value>> {
    let u_scope = user_scope(guard.user_id);

    let head_rows = sqlx::query(
        "SELECT scope, slot_key, slot_value, current_rev, updated_at
         FROM agent_ltm_head
         WHERE space_id = ?1 AND scope IN ('shared', ?2)
         ORDER BY scope ASC, updated_at DESC",
    )
    .bind(guard.space_id)
    .bind(&u_scope)
    .fetch_all(&state.db.read)
    .await?;

    let head: Vec<Value> = head_rows
        .into_iter()
        .map(|r| {
            json!({
                "scope": r.get::<String, _>("scope"),
                "slot_key": r.get::<String, _>("slot_key"),
                "slot_value": r.get::<String, _>("slot_value"),
                "current_rev": r.get::<String, _>("current_rev"),
                "updated_at": r.get::<i64, _>("updated_at"),
            })
        })
        .collect();

    let commit_rows = sqlx::query(
        "SELECT rev_id, scope, parent_rev, source_session_id, message, ops_json, created_at
         FROM agent_ltm_commits
         WHERE space_id = ?1 AND scope IN ('shared', ?2)
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(guard.space_id)
    .bind(&u_scope)
    .fetch_all(&state.db.read)
    .await?;

    let commits: Vec<LtmCommitDto> = commit_rows
        .into_iter()
        .map(|r| LtmCommitDto {
            rev_id: r.get("rev_id"),
            scope: r.get("scope"),
            parent_rev: r.get("parent_rev"),
            source_session_id: r.get("source_session_id"),
            message: r.get("message"),
            ops: serde_json::from_str(r.get("ops_json")).unwrap_or_default(),
            created_at: r.get("created_at"),
        })
        .collect();

    let stm_rows = sqlx::query(
        "SELECT id, scope, source_session_id, summary_text, promoted_to_ltm, created_at
         FROM agent_stm_segments
         WHERE space_id = ?1 AND scope IN ('shared', ?2) AND archived_at IS NULL
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(guard.space_id)
    .bind(&u_scope)
    .fetch_all(&state.db.read)
    .await?;

    let stm: Vec<Value> = stm_rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.get::<i64, _>("id").to_string(),
                "scope": r.get::<String, _>("scope"),
                "source_session_id": r.get::<String, _>("source_session_id"),
                "summary_text": r.get::<String, _>("summary_text"),
                "promoted_to_ltm": r.get::<i64, _>("promoted_to_ltm") != 0,
                "created_at": r.get::<i64, _>("created_at"),
            })
        })
        .collect();

    let shared_markdown = render_scope_ltm_markdown(&state, guard.space_id, "shared")
        .await
        .unwrap_or_default();
    let private_markdown = render_scope_ltm_markdown(&state, guard.space_id, &u_scope)
        .await
        .unwrap_or_default();

    Ok(Json(json!({
        "ltm_head": head,
        "ltm_commits": commits,
        "stm_recent": stm,
        "shared_markdown": shared_markdown,
        "private_markdown": private_markdown,
    })))
}

async fn get_memory_markdown_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<Value>> {
    get_memory_inspector(guard, State(state)).await
}

#[derive(Deserialize)]
pub struct UpdateMemoryMarkdownReq {
    /// `"shared"` | `"private"`
    pub scope: String,
    pub markdown: String,
}

async fn put_memory_markdown_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<UpdateMemoryMarkdownReq>,
) -> AppResult<Json<Value>> {
    if req.markdown.len() > 24_000 {
        return Err(AppError::BadRequest("记忆文档单页不超过 24,000 字符".to_string()));
    }
    let resolved_scope = match req.scope.trim() {
        "shared" => "shared".to_string(),
        "private" => user_scope(guard.user_id),
        _ => {
            return Err(AppError::BadRequest(
                "无效的记忆作用域（仅支持 shared 或 private）".to_string(),
            ))
        }
    };

    let commit = sync_scope_ltm_from_markdown(
        &state,
        guard.space_id,
        &resolved_scope,
        &req.markdown,
    )
    .await
    .map_err(AppError::Internal)?;

    if resolved_scope == "shared" && commit.is_some() {
        state
            .hub
            .publish(
                guard.space_id,
                "AGENT_MEMORY_UPDATED",
                json!({ "scope": "shared" }),
            )
            .await;
    }

    get_memory_inspector(guard, State(state)).await
}

async fn revert_ltm_commit_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Path((_space_id, rev_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let u_scope = user_scope(guard.user_id);
    let row = sqlx::query(
        "SELECT scope, ops_json FROM agent_ltm_commits
         WHERE rev_id = ?1 AND space_id = ?2 AND scope IN ('shared', ?3)",
    )
    .bind(&rev_id)
    .bind(guard.space_id)
    .bind(&u_scope)
    .fetch_optional(&state.db.read)
    .await?
    .ok_or_else(|| AppError::NotFound("未找到可回滚的记忆变更版本".to_string()))?;

    let scope: String = row.get("scope");
    let ops: Vec<LtmDiffOp> = serde_json::from_str(row.get("ops_json")).unwrap_or_default();
    let inverse_ops: Vec<LtmDiffOp> = ops
        .into_iter()
        .rev()
        .map(|op| match op.op.as_str() {
            "ADD" => LtmDiffOp {
                op: "DELETE".to_string(),
                slot_key: op.slot_key,
                old_val: op.new_val,
                new_val: None,
                reason: format!("revert {rev_id}"),
            },
            _ => LtmDiffOp {
                op: if op.old_val.is_some() {
                    "REPLACE".to_string()
                } else {
                    "DELETE".to_string()
                },
                slot_key: op.slot_key,
                old_val: op.new_val,
                new_val: op.old_val,
                reason: format!("revert {rev_id}"),
            },
        })
        .collect();

    let reverted = apply_ltm_git_commit(
        &state,
        guard.space_id,
        &scope,
        "manual_revert",
        &format!("Revert commit {rev_id}"),
        inverse_ops,
    )
    .await
    .map_err(AppError::Internal)?;

    if scope == "shared" {
        state
            .hub
            .publish(
                guard.space_id,
                "AGENT_MEMORY_UPDATED",
                json!({ "scope": "shared" }),
            )
            .await;
    }

    Ok(Json(json!({ "reverted_commit": reverted })))
}

async fn get_providers_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<Value>> {
    let mut list = list_configured_providers(&state, guard.space_id).await;
    for p in &mut list {
        p.api_key = mask_secret(&p.api_key);
    }
    Ok(Json(json!({ "providers": list })))
}

fn mask_secret(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 8 {
        "••••••••".to_string()
    } else {
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}••••{tail}")
    }
}

#[derive(Deserialize)]
pub struct UpsertProviderReq {
    pub id: Option<String>,
    #[serde(default)]
    pub vendor: String,
    pub name: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub chat_model: String,
    #[serde(default)]
    pub memory_model: String,
    #[serde(default)]
    pub is_chat_primary: bool,
    #[serde(default)]
    pub is_memory_primary: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_prio")]
    pub priority: i64,
}
fn default_enabled() -> bool {
    true
}
fn default_prio() -> i64 {
    50
}

async fn upsert_provider_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<UpsertProviderReq>,
) -> AppResult<Json<Value>> {
    let preset = resolve_vendor_preset(if !req.vendor.trim().is_empty() {
        &req.vendor
    } else {
        &req.name
    });

    let effective_name = if !req.name.trim().is_empty() {
        req.name.trim().to_string()
    } else if let Some(pr) = preset {
        pr.display_name.to_string()
    } else {
        return Err(AppError::BadRequest("请选择一个模型供应商".to_string()));
    };

    let effective_proto = if !req.protocol.trim().is_empty() {
        req.protocol.trim().to_string()
    } else if let Some(pr) = preset {
        pr.protocol.to_string()
    } else {
        "openai_compat".to_string()
    };

    if !matches!(
        effective_proto.as_str(),
        "openai_compat" | "responses" | "anthropic" | "gemini"
    ) {
        return Err(AppError::BadRequest(
            "不支持的协议类型（支持 openai_compat / anthropic / gemini）".to_string(),
        ));
    }

    let effective_base_url = if !req.base_url.trim().is_empty() {
        req.base_url.trim().to_string()
    } else if let Some(pr) = preset {
        pr.base_url.to_string()
    } else {
        return Err(AppError::BadRequest(
            "未能识别该供应商默认网关，请重新选择预设供应商".to_string(),
        ));
    };

    let effective_chat_model = if !req.chat_model.trim().is_empty() {
        req.chat_model.trim().to_string()
    } else if let Some(pr) = preset {
        pr.default_chat_model.to_string()
    } else {
        "gpt-4o".to_string()
    };

    let effective_memory_model = if !req.memory_model.trim().is_empty() {
        req.memory_model.trim().to_string()
    } else if let Some(pr) = preset {
        pr.default_memory_model.to_string()
    } else {
        effective_chat_model.clone()
    };

    // Upsert by explicit `id`, OR reuse existing space row for the same vendor display name so re-pasting a token updates cleanly instead of duplicating!
    let pid = if let Some(explicit_id) = req
        .id
        .as_ref()
        .filter(|s| !s.trim().is_empty() && !s.starts_with("env_"))
    {
        explicit_id.trim().to_string()
    } else if let Some(existing_id) = sqlx::query_scalar::<_, String>(
        "SELECT id FROM agent_providers WHERE space_id = ?1 AND LOWER(name) = LOWER(?2) LIMIT 1",
    )
    .bind(guard.space_id)
    .bind(&effective_name)
    .fetch_optional(&state.db.read)
    .await?
    {
        existing_id
    } else {
        format!("prov_{}", state.snowflake.next_id())
    };

    // Preserve existing key if UI sent back masked placeholder `••••` or empty when toggling an existing provider
    let final_key = if req.api_key.contains("••••") || req.api_key.trim().is_empty() {
        sqlx::query_scalar::<_, String>(
            "SELECT api_key FROM agent_providers WHERE id = ?1 AND space_id = ?2",
        )
        .bind(&pid)
        .bind(guard.space_id)
        .fetch_optional(&state.db.read)
        .await?
        .ok_or_else(|| AppError::BadRequest("首次启用请填入真实 API Token".to_string()))?
    } else {
        req.api_key.trim().to_string()
    };

    if req.is_chat_primary {
        let _ = sqlx::query(
            "UPDATE agent_providers SET is_chat_primary = 0, priority = MAX(priority, 20) WHERE space_id = ?1",
        )
        .bind(guard.space_id)
        .execute(&state.db.write)
        .await;
    }
    if req.is_memory_primary {
        let _ = sqlx::query("UPDATE agent_providers SET is_memory_primary = 0 WHERE space_id = ?1")
            .bind(guard.space_id)
            .execute(&state.db.write)
            .await;
    }

    sqlx::query(
        "INSERT INTO agent_providers
         (id, space_id, name, protocol, base_url, api_key, chat_model, memory_model, is_chat_primary, is_memory_primary, enabled, priority, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            protocol = excluded.protocol,
            base_url = excluded.base_url,
            api_key = excluded.api_key,
            chat_model = excluded.chat_model,
            memory_model = excluded.memory_model,
            is_chat_primary = excluded.is_chat_primary,
            is_memory_primary = excluded.is_memory_primary,
            enabled = excluded.enabled,
            priority = excluded.priority,
            updated_at = excluded.updated_at",
    )
    .bind(&pid)
    .bind(guard.space_id)
    .bind(&effective_name)
    .bind(&effective_proto)
    .bind(&effective_base_url)
    .bind(&final_key)
    .bind(&effective_chat_model)
    .bind(&effective_memory_model)
    .bind(if req.is_chat_primary { 1 } else { 0 })
    .bind(if req.is_memory_primary { 1 } else { 0 })
    .bind(if req.enabled { 1 } else { 0 })
    .bind(if req.is_chat_primary { 5 } else { req.priority })
    .bind(now_ms())
    .execute(&state.db.write)
    .await?;

    if req.enabled {
        probe_and_cache_provider(&state, guard.space_id, &pid).await;
    }

    state
        .hub
        .publish(guard.space_id, "AGENT_CONFIG_UPDATED", json!({}))
        .await;

    get_providers_handler(guard, State(state)).await
}

#[derive(Deserialize)]
pub struct FetchVendorModelsReq {
    pub vendor: String,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

async fn fetch_vendor_models_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<FetchVendorModelsReq>,
) -> AppResult<Json<Value>> {
    let res = fetch_live_vendor_models(
        &state,
        guard.space_id,
        &req.vendor,
        req.provider_id.as_deref(),
        req.api_key.as_deref(),
    )
    .await;
    Ok(Json(serde_json::to_value(res).unwrap_or_default()))
}

async fn probe_provider_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Path((_space_id, provider_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    probe_and_cache_provider(&state, guard.space_id, &provider_id).await;
    state
        .hub
        .publish(guard.space_id, "AGENT_CONFIG_UPDATED", json!({}))
        .await;
    get_providers_handler(guard, State(state)).await
}

async fn activate_provider_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Path((_space_id, provider_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    if !provider_id.starts_with("env_") {
        let updated = sqlx::query(
            "UPDATE agent_providers
             SET is_chat_primary = CASE WHEN id = ?1 THEN 1 ELSE 0 END,
                 is_memory_primary = CASE WHEN id = ?1 THEN 1 ELSE 0 END,
                 enabled = CASE WHEN id = ?1 THEN 1 ELSE enabled END,
                 priority = CASE WHEN id = ?1 THEN 5 ELSE MAX(priority, 20) END,
                 updated_at = CASE WHEN id = ?1 THEN ?3 ELSE updated_at END
             WHERE space_id = ?2",
        )
        .bind(&provider_id)
        .bind(guard.space_id)
        .bind(now_ms())
        .execute(&state.db.write)
        .await?;

        if updated.rows_affected() == 0 {
            return Err(AppError::NotFound("未找到指定的模型供应商配置".to_string()));
        }
    }

    probe_and_cache_provider(&state, guard.space_id, &provider_id).await;

    state
        .hub
        .publish(guard.space_id, "AGENT_CONFIG_UPDATED", json!({}))
        .await;

    get_providers_handler(guard, State(state)).await
}

async fn delete_provider_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Path((_space_id, provider_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    sqlx::query("DELETE FROM agent_providers WHERE id = ?1 AND space_id = ?2")
        .bind(&provider_id)
        .bind(guard.space_id)
        .execute(&state.db.write)
        .await?;

    // If no primary remains, promote the highest-priority enabled provider automatically
    let has_prim: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM agent_providers WHERE space_id = ?1 AND enabled = 1 AND is_chat_primary = 1 LIMIT 1",
    )
    .bind(guard.space_id)
    .fetch_optional(&state.db.read)
    .await?;
    if has_prim.is_none() {
        let _ = sqlx::query(
            "UPDATE agent_providers
             SET is_chat_primary = 1, is_memory_primary = 1, priority = 5
             WHERE id = (
                 SELECT id FROM agent_providers
                 WHERE space_id = ?1 AND enabled = 1
                 ORDER BY priority ASC, updated_at DESC LIMIT 1
             )",
        )
        .bind(guard.space_id)
        .execute(&state.db.write)
        .await;
    }

    state
        .hub
        .publish(guard.space_id, "AGENT_CONFIG_UPDATED", json!({}))
        .await;

    get_providers_handler(guard, State(state)).await
}

async fn get_mcp_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<Value>> {
    let servers = load_mcp_servers(&state, guard.space_id).await;
    Ok(Json(json!({ "mcp_servers": servers })))
}

async fn upsert_mcp_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<McpServerConfigDto>,
) -> AppResult<Json<Value>> {
    let mid = if req.id.trim().is_empty() {
        format!("mcp_{}", state.snowflake.next_id())
    } else {
        req.id.trim().to_string()
    };
    sqlx::query(
        "INSERT INTO agent_mcp_servers
         (id, space_id, name, description, transport, endpoint, args_json, headers_json, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            description = excluded.description,
            transport = excluded.transport,
            endpoint = excluded.endpoint,
            args_json = excluded.args_json,
            headers_json = excluded.headers_json,
            enabled = excluded.enabled,
            updated_at = excluded.updated_at",
    )
    .bind(&mid)
    .bind(guard.space_id)
    .bind(req.name.trim())
    .bind(req.description.trim())
    .bind(req.transport.trim())
    .bind(req.endpoint.trim())
    .bind(serde_json::to_string(&req.args).unwrap_or_else(|_| "[]".into()))
    .bind(serde_json::to_string(&req.headers).unwrap_or_else(|_| "{}".into()))
    .bind(if req.enabled { 1 } else { 0 })
    .bind(now_ms())
    .execute(&state.db.write)
    .await?;
    invalidate_mcp_cache(guard.space_id, Some(&mid));
    state
        .hub
        .publish(guard.space_id, "AGENT_CONFIG_UPDATED", json!({}))
        .await;
    get_mcp_handler(guard, State(state)).await
}

async fn probe_mcp_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Path((_space_id, mcp_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    probe_single_mcp_server(&state, guard.space_id, &mcp_id).await;
    state
        .hub
        .publish(guard.space_id, "AGENT_CONFIG_UPDATED", json!({}))
        .await;
    get_mcp_handler(guard, State(state)).await
}

async fn delete_mcp_handler(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Path((_space_id, mcp_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    invalidate_mcp_cache(guard.space_id, Some(&mcp_id));
    sqlx::query("DELETE FROM agent_mcp_servers WHERE id = ?1 AND space_id = ?2")
        .bind(&mcp_id)
        .bind(guard.space_id)
        .execute(&state.db.write)
        .await?;
    state
        .hub
        .publish(guard.space_id, "AGENT_CONFIG_UPDATED", json!({}))
        .await;
    get_mcp_handler(guard, State(state)).await
}

/// Invoked every 60s from `spawn_maintenance_worker`:
/// 1. Seals idle sessions (`last_active_at < now - 30m`) -> flushes S3 -> extracts STM
/// 2. Retries any sealed sessions whose STM wasn't extracted yet
/// 3. Promotes STM -> Git-style LTM commits
/// 4. GCs >7-day STM segments ONLY when `promoted_to_ltm == 1`
pub async fn tick_agent_maintenance(state: &AppState) {
    let idle_cutoff = now_ms() - SESSION_IDLE_SEAL_MS;
    if let Ok(rows) = sqlx::query(
        "SELECT session_id, space_id, user_id FROM agent_sessions
         WHERE status = 'ACTIVE' AND last_active_at < ?1
         LIMIT 8",
    )
    .bind(idle_cutoff)
    .fetch_all(&state.db.read)
    .await
    {
        for r in rows {
            let sid: String = r.get("session_id");
            let sp: i64 = r.get("space_id");
            let uid: i64 = r.get("user_id");
            seal_session_and_spawn_extract(state.clone(), sp, uid, sid).await;
        }
    }

    if let Ok(unextracted) = sqlx::query(
        "SELECT session_id, space_id, user_id FROM agent_sessions
         WHERE status = 'SEALED' AND stm_extracted = 0
         LIMIT 4",
    )
    .fetch_all(&state.db.read)
    .await
    {
        for r in unextracted {
            let sid: String = r.get("session_id");
            let sp: i64 = r.get("space_id");
            let uid: i64 = r.get("user_id");
            let _ = extract_sealed_session_to_stm(state, sp, uid, &sid).await;
        }
    }

    promote_stm_to_ltm(state).await;
    gc_expired_stm_segments(state).await;
}
