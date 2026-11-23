use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashSet;

use crate::{
    agent::{
        a2ui::A2uiSurfacePayload,
        provider::{complete_with_failover, LlmMessage, ModelRole},
    },
    state::AppState,
    time_bj::now_ms,
};

pub const SESSION_IDLE_SEAL_MS: i64 = 30 * 60 * 1000; // 30 minutes natural silence seals session
pub const STM_RETENTION_MS: i64 = 7 * 24 * 3600 * 1000; // 7-day rolling STM window
pub const CLARIFY_COOLDOWN_MS: i64 = 24 * 3600 * 1000; // Max 1 piggyback question per 24h
pub const LTM_SHARED_CHAR_BUDGET: usize = 2600; // ~1500 CJK/mixed tokens
pub const LTM_PRIVATE_CHAR_BUDGET: usize = 1800; // ~1000 CJK/mixed tokens

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTraceSummary {
    pub call_id: String,
    pub tool_name: String,
    pub ok: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTurnRecord {
    pub turn_id: String,
    /// `"user"` | `"assistant"`
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub ui_action_summary: Option<String>,
    #[serde(default)]
    pub tool_traces: Vec<ToolTraceSummary>,
    #[serde(default)]
    pub surfaces: Vec<A2uiSurfacePayload>,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetaDto {
    pub session_id: String,
    pub title: String,
    pub status: String,
    pub turn_count: i64,
    pub last_active_at: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StmRecallHit {
    pub segment_id: i64,
    pub scope: String,
    pub source_session_id: String,
    pub summary_text: String,
    pub score: f64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtmDiffOp {
    /// `"ADD"` | `"REPLACE"` | `"DELETE"`
    pub op: String,
    pub slot_key: String,
    #[serde(default)]
    pub old_val: Option<String>,
    #[serde(default)]
    pub new_val: Option<String>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LtmCommitDto {
    pub rev_id: String,
    pub scope: String,
    pub parent_rev: Option<String>,
    pub source_session_id: String,
    pub message: String,
    pub ops: Vec<LtmDiffOp>,
    pub created_at: i64,
}

pub fn user_scope(user_id: i64) -> String {
    format!("user:{user_id}")
}

pub fn s3_session_turns_key(space_id: i64, user_id: i64, session_id: &str) -> String {
    format!("agent/spaces/{space_id}/sessions/{user_id}/{session_id}/turns.jsonl")
}

pub fn s3_session_meta_key(space_id: i64, user_id: i64, session_id: &str) -> String {
    format!("agent/spaces/{space_id}/sessions/{user_id}/{session_id}/meta.json")
}

/// Zero-RAM Chinese Character Bigram + Alphanumeric Tokenizer for SQLite FTS5 `bm25()`.
pub fn segment_for_bm25(input: &str) -> String {
    let mut tokens: Vec<String> = Vec::new();
    let mut ascii_buf = String::new();
    let mut cjk_run: Vec<char> = Vec::new();

    let flush_ascii = |buf: &mut String, out: &mut Vec<String>| {
        if !buf.is_empty() {
            out.push(buf.to_ascii_lowercase());
            buf.clear();
        }
    };
    let flush_cjk = |run: &mut Vec<char>, out: &mut Vec<String>| {
        if run.len() == 1 {
            out.push(run[0].to_string());
        } else if run.len() >= 2 {
            for win in run.windows(2) {
                let mut bi = String::with_capacity(6);
                bi.push(win[0]);
                bi.push(win[1]);
                out.push(bi);
            }
        }
        run.clear();
    };

    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            flush_cjk(&mut cjk_run, &mut tokens);
            ascii_buf.push(ch);
        } else if is_cjk(ch) {
            flush_ascii(&mut ascii_buf, &mut tokens);
            cjk_run.push(ch);
        } else {
            flush_ascii(&mut ascii_buf, &mut tokens);
            flush_cjk(&mut cjk_run, &mut tokens);
        }
    }
    flush_ascii(&mut ascii_buf, &mut tokens);
    flush_cjk(&mut cjk_run, &mut tokens);

    tokens.join(" ")
}

fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x3040..=0x30FF | 0xAC00..=0xD7AF
    )
}

/// Builds a safe FTS5 `OR` query string from user utterance using `agent-kernel`'s
/// CJK-aware [`xai_grok_memory::query_expansion::extract_keywords`].
fn build_fts5_match_expr(query: &str) -> Option<String> {
    let mut uniq = Vec::new();
    let mut seen = HashSet::new();

    for tok in xai_grok_memory::query_expansion::extract_keywords(query)
        .into_iter()
        .chain(segment_for_bm25(query).split_whitespace().map(str::to_string))
    {
        let clean: String = tok
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !clean.is_empty() && seen.insert(clean.clone()) {
            uniq.push(format!("\"{clean}\""));
            if uniq.len() >= 24 {
                break;
            }
        }
    }
    if uniq.is_empty() {
        None
    } else {
        Some(uniq.join(" OR "))
    }
}

/// Powered directly by `agent-kernel` (`xai-grok-memory`):
/// SQLite FTS5 Okapi `bm25()` + [`xai_grok_memory::search::temporal_decay_multiplier`] (half-life 3.5d)
/// + [`xai_grok_memory::mmr::mmr_rerank`] (`lambda = 0.68`, CJK bigram Jaccard) strictly isolated to `(shared ∪ user:{self})`.
pub async fn recall_stm_segments(
    state: &AppState,
    space_id: i64,
    user_id: i64,
    query: &str,
    top_k: usize,
) -> Vec<StmRecallHit> {
    let Some(match_expr) = build_fts5_match_expr(query) else {
        return Vec::new();
    };
    let shared_tag = format!("{space_id}:shared");
    let priv_tag = format!("{space_id}:{}", user_scope(user_id));

    let rows = match sqlx::query(
        "SELECT s.id, s.scope, s.source_session_id, s.summary_text, s.keywords_seg, s.created_at,
                bm25(agent_stm_fts) AS raw_bm25
         FROM agent_stm_fts
         JOIN agent_stm_segments s ON s.id = CAST(agent_stm_fts.segment_id AS INTEGER)
         WHERE agent_stm_fts MATCH ?1
           AND agent_stm_fts.space_scope IN (?2, ?3)
           AND s.archived_at IS NULL
         ORDER BY raw_bm25 ASC
         LIMIT ?4",
    )
    .bind(&match_expr)
    .bind(&shared_tag)
    .bind(&priv_tag)
    .bind((top_k * 4) as i64)
    .fetch_all(&state.db.read)
    .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    if rows.is_empty() {
        return Vec::new();
    }

    let now_secs = now_ms() / 1000;
    let mut search_results: Vec<xai_grok_memory::search::SearchResult> = Vec::with_capacity(rows.len());
    let mut relevances: Vec<f64> = Vec::with_capacity(rows.len());
    let mut hit_by_id: std::collections::HashMap<String, StmRecallHit> =
        std::collections::HashMap::with_capacity(rows.len());

    for r in rows {
        let seg_id: i64 = r.get("id");
        let chunk_key = seg_id.to_string();
        let raw_bm25: f64 = r.try_get("raw_bm25").unwrap_or(0.0);
        let bm25_pos = (-raw_bm25).max(0.01);
        let lexical = bm25_pos / (1.0 + bm25_pos);

        let created_at_ms: i64 = r.get("created_at");
        let decay = xai_grok_memory::search::temporal_decay_multiplier(
            "session",
            created_at_ms / 1000,
            now_secs,
            Some(3.5),
        );
        let relevance = lexical * (0.35 + 0.65 * decay);

        let scope: String = r.get("scope");
        let source_session_id: String = r.get("source_session_id");
        let summary_text: String = r.get("summary_text");

        search_results.push(xai_grok_memory::search::SearchResult {
            chunk_id: chunk_key.clone(),
            path: source_session_id.clone(),
            start_line: 0,
            end_line: 1,
            score: relevance,
            snippet: summary_text.clone(),
            source: scope.clone(),
            created_at: created_at_ms / 1000,
        });
        relevances.push(relevance);
        hit_by_id.insert(
            chunk_key,
            StmRecallHit {
                segment_id: seg_id,
                scope,
                source_session_id,
                summary_text,
                score: relevance,
                created_at: created_at_ms,
            },
        );
    }

    xai_grok_memory::mmr::mmr_rerank(
        &mut search_results,
        &relevances,
        &xai_grok_config_types::MmrConfig {
            enabled: true,
            lambda: 0.68,
        },
    );

    search_results
        .into_iter()
        .take(top_k)
        .filter_map(|sr| hit_by_id.remove(&sr.chunk_id))
        .collect()
}

/// Persists the complete `turns.jsonl` + `meta.json` of a session to S3.
pub async fn flush_session_to_s3(
    state: &AppState,
    space_id: i64,
    user_id: i64,
    session_id: &str,
    title: &str,
    status: &str,
    turns: &[SessionTurnRecord],
) -> Result<(), String> {
    let mut jsonl = String::new();
    for t in turns {
        if let Ok(line) = serde_json::to_string(t) {
            jsonl.push_str(&line);
            jsonl.push('\n');
        }
    }
    let turns_key = s3_session_turns_key(space_id, user_id, session_id);
    state
        .s3
        .put_bytes(&turns_key, "application/x-ndjson", jsonl.as_bytes())
        .await?;

    let meta = json!({
        "session_id": session_id,
        "space_id": space_id.to_string(),
        "speaker_user_id": user_id.to_string(),
        "title": title,
        "status": status,
        "turn_count": turns.len(),
        "updated_at": now_ms(),
    });
    let meta_key = s3_session_meta_key(space_id, user_id, session_id);
    let meta_bytes = serde_json::to_vec(&meta).unwrap_or_default();
    state
        .s3
        .put_bytes(&meta_key, "application/json", &meta_bytes)
        .await?;
    Ok(())
}

/// Hydrates session turns directly from S3 `turns.jsonl`.
pub async fn load_session_turns_from_s3(
    state: &AppState,
    space_id: i64,
    user_id: i64,
    session_id: &str,
) -> Vec<SessionTurnRecord> {
    let key = s3_session_turns_key(space_id, user_id, session_id);
    let Ok(Some(bytes)) = state.s3.get_bytes(&key).await else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<SessionTurnRecord>(l).ok())
        .collect()
}

impl xai_grok_compaction::CompactionItem for SessionTurnRecord {
    fn role(&self) -> xai_grok_compaction::CompactionRole {
        if self.role == "user" {
            xai_grok_compaction::CompactionRole::User
        } else {
            xai_grok_compaction::CompactionRole::Assistant
        }
    }

    fn text(&self) -> Option<String> {
        Some(self.text.clone())
    }

    fn has_tool_requests(&self) -> bool {
        !self.tool_traces.is_empty()
    }

    fn is_compaction_summary(&self) -> bool {
        false
    }

    fn attachment_refs(&self) -> Vec<xai_grok_compaction::CompactionFileRef> {
        Vec::new()
    }
}

/// In-session rolling compaction powered by `agent-kernel`'s [`xai_grok_compaction::select_turns_to_compact`]
/// (NO 40-turn hard cut!). Tool-pair-safe tail-keep selection keeps recent turns verbatim within
/// budget (`target_tokens = 3600`) and compacts older history into `rolling_summary` when `min_compactable >= 2400`.
pub async fn maybe_roll_compact_session(
    state: &AppState,
    space_id: i64,
    session_id: &str,
    existing_rolling: &str,
    compacted_turn_count: usize,
    turns: &[SessionTurnRecord],
) -> (String, usize) {
    let start = compacted_turn_count.min(turns.len());
    let uncompacted = &turns[start..];
    if uncompacted.len() <= 20 {
        return (existing_rolling.to_string(), start);
    }
    let token_counts: Vec<u32> = uncompacted
        .iter()
        .map(|t| ((t.text.chars().count() as u32).saturating_add(12)) / 2)
        .collect();

    let Some(split_plan) =
        xai_grok_compaction::select_turns_to_compact(&token_counts, uncompacted, 1_800, 1_200)
    else {
        return (existing_rolling.to_string(), start);
    };
    if split_plan.split_idx == 0 {
        return (existing_rolling.to_string(), start);
    }

    let older_slice = &uncompacted[..split_plan.split_idx];
    let mut transcript = String::new();
    if !existing_rolling.trim().is_empty() {
        transcript.push_str("【既有前情提要】\n");
        transcript.push_str(existing_rolling);
        transcript.push_str("\n\n");
    }
    for t in older_slice {
        transcript.push_str(&format!("[{}]: {}\n", t.role, t.text));
    }

    let sys = "你是会话上下文无损折叠器。请将下面较早的对话浓缩为 300 字以内的关键事实、上下文脉络与未完事项摘要，确保后续对话衔接自然不断裂。直接输出摘要正文。";
    if let Ok(comp) = complete_with_failover(
        state,
        space_id,
        ModelRole::Memory,
        sys,
        "",
        &[LlmMessage::User {
            content: transcript,
        }],
        &[],
    )
    .await
    {
        if comp.provider_used == "builtin:local" {
            return (existing_rolling.to_string(), start);
        }
        let next = comp.text.trim().to_string();
        if !next.is_empty() {
            let new_cursor = start + split_plan.split_idx;
            let _ = sqlx::query(
                "UPDATE agent_sessions
                 SET rolling_summary = ?1, compacted_turn_count = ?2
                 WHERE session_id = ?3",
            )
            .bind(&next)
            .bind(new_cursor as i64)
            .bind(session_id)
            .execute(&state.db.write)
            .await;
            return (next, new_cursor);
        }
    }
    (existing_rolling.to_string(), start)
}

/// Renders Long-Term Memory (`Shared` + `User Private` ONLY) to place immediately after System Prompt.
pub async fn render_ltm_for_system_prompt(state: &AppState, space_id: i64, user_id: i64) -> String {
    let u_scope = user_scope(user_id);
    let rows = match sqlx::query(
        "SELECT scope, slot_key, slot_value
         FROM agent_ltm_head
         WHERE space_id = ?1 AND scope IN ('shared', ?2)
         ORDER BY scope ASC, slot_key ASC",
    )
    .bind(space_id)
    .bind(&u_scope)
    .fetch_all(&state.db.read)
    .await
    {
        Ok(r) => r,
        Err(_) => return String::new(),
    };

    if rows.is_empty() {
        return String::new();
    }

    let mut shared_lines = Vec::new();
    let mut priv_lines = Vec::new();
    for r in rows {
        let scope: String = r.get("scope");
        let k: String = r.get("slot_key");
        let v: String = r.get("slot_value");
        let formatted = if k.starts_with("md_") {
            format!("- {v}")
        } else {
            format!("- [{k}]: {v}")
        };
        if scope == "shared" {
            shared_lines.push(formatted);
        } else {
            priv_lines.push(formatted);
        }
    }

    let mut out = String::from("## 长期记忆（紧跟系统人设 · 稳定前缀）\n");
    if !shared_lines.is_empty() {
        out.push_str("### 【家庭公共记忆（双方共享事实）】\n");
        out.push_str(&shared_lines.join("\n"));
        out.push('\n');
    }
    if !priv_lines.is_empty() {
        out.push_str("### 【当前对话人专属私密记忆（绝对禁止向另一半透露）】\n");
        out.push_str(&priv_lines.join("\n"));
        out.push('\n');
    }
    out
}

/// Peeks at most 1 pending ambiguous question if the user hasn't been asked in the last 24 hours.
/// Call [`mark_clarification_asked`] ONLY after the turn finishes streaming so failed/cancelled turns don't burn it!
pub async fn peek_piggyback_clarification(
    state: &AppState,
    space_id: i64,
    user_id: i64,
) -> Option<(i64, String)> {
    let now = now_ms();
    let recent_asked: Option<i64> = sqlx::query_scalar(
        "SELECT asked_at FROM agent_clarifications
         WHERE space_id = ?1 AND user_id = ?2 AND asked_at IS NOT NULL
         ORDER BY asked_at DESC LIMIT 1",
    )
    .bind(space_id)
    .bind(user_id)
    .fetch_optional(&state.db.read)
    .await
    .ok()
    .flatten();

    if let Some(last_ts) = recent_asked {
        if now - last_ts < CLARIFY_COOLDOWN_MS {
            return None;
        }
    }

    let row = sqlx::query(
        "SELECT id, question_hint FROM agent_clarifications
         WHERE space_id = ?1 AND user_id = ?2 AND status = 'PENDING'
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(space_id)
    .bind(user_id)
    .fetch_optional(&state.db.read)
    .await
    .ok()
    .flatten()?;

    let cid: i64 = row.get("id");
    let hint: String = row.get("question_hint");
    Some((cid, hint))
}

pub async fn mark_clarification_asked(state: &AppState, clarification_id: i64) {
    let _ = sqlx::query(
        "UPDATE agent_clarifications
         SET status = 'ASKED', asked_at = ?1
         WHERE id = ?2 AND status = 'PENDING'",
    )
    .bind(now_ms())
    .bind(clarification_id)
    .execute(&state.db.write)
    .await;
}

/// Applies a Git-Style atomic Commit (`parent_rev` -> `rev_id`) onto `agent_ltm_head` & `agent_ltm_commits`,
/// and appends the commit entry to S3 `commits.jsonl`.
pub async fn apply_ltm_git_commit(
    state: &AppState,
    space_id: i64,
    scope: &str,
    source_session_id: &str,
    message: &str,
    ops: Vec<LtmDiffOp>,
) -> Result<Option<LtmCommitDto>, String> {
    if ops.is_empty() {
        return Ok(None);
    }
    let _guard = state.agent.ltm_commit_lock.lock().await;
    let now = now_ms();
    let rev_id = format!("rev_{}", state.snowflake.next_id());

    let mut tx = state
        .db
        .write
        .begin()
        .await
        .map_err(|e| format!("开启 LTM 事务失败: {e}"))?;

    let parent_rev: Option<String> = sqlx::query_scalar(
        "SELECT rev_id FROM agent_ltm_commits
         WHERE space_id = ?1 AND scope = ?2
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(space_id)
    .bind(scope)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    let mut applied_ops = Vec::new();
    for mut op in ops {
        let key = op.slot_key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT slot_value FROM agent_ltm_head
             WHERE space_id = ?1 AND scope = ?2 AND slot_key = ?3",
        )
        .bind(space_id)
        .bind(scope)
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

        op.old_val = existing.clone();
        match op.op.to_ascii_uppercase().as_str() {
            "DELETE" => {
                if existing.is_some() {
                    sqlx::query(
                        "DELETE FROM agent_ltm_head WHERE space_id = ?1 AND scope = ?2 AND slot_key = ?3",
                    )
                    .bind(space_id)
                    .bind(scope)
                    .bind(&key)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
                    op.op = "DELETE".to_string();
                    applied_ops.push(op);
                }
            }
            _ => {
                let Some(new_v) = op.new_val.as_ref().map(|s| s.trim().to_string()) else {
                    continue;
                };
                if new_v.is_empty() || existing.as_deref() == Some(new_v.as_str()) {
                    continue;
                }
                op.op = if existing.is_some() {
                    "REPLACE".to_string()
                } else {
                    "ADD".to_string()
                };
                op.new_val = Some(new_v.clone());
                sqlx::query(
                    "INSERT INTO agent_ltm_head (space_id, scope, slot_key, slot_value, current_rev, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(space_id, scope, slot_key) DO UPDATE SET
                        slot_value = excluded.slot_value,
                        current_rev = excluded.current_rev,
                        updated_at = excluded.updated_at",
                )
                .bind(space_id)
                .bind(scope)
                .bind(&key)
                .bind(&new_v)
                .bind(&rev_id)
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
                applied_ops.push(op);
            }
        }
    }

    if applied_ops.is_empty() {
        return Ok(None);
    }

    // Enforce hard char/token ceiling on HEAD BEFORE writing commit log so pruned slots are recorded in `ops_json` (reversible!)
    let cap = if scope == "shared" {
        LTM_SHARED_CHAR_BUDGET
    } else {
        LTM_PRIVATE_CHAR_BUDGET
    };
    let head_rows = sqlx::query(
        "SELECT slot_key, slot_value FROM agent_ltm_head
         WHERE space_id = ?1 AND scope = ?2
         ORDER BY updated_at DESC, rowid DESC",
    )
    .bind(space_id)
    .bind(scope)
    .fetch_all(&mut *tx)
    .await
    .unwrap_or_default();

    let mut used = 0usize;
    for r in head_rows {
        let k: String = r.get("slot_key");
        let v: String = r.get("slot_value");
        used += k.chars().count() + v.chars().count() + 6;
        if used > cap {
            let _ = sqlx::query(
                "DELETE FROM agent_ltm_head WHERE space_id = ?1 AND scope = ?2 AND slot_key = ?3",
            )
            .bind(space_id)
            .bind(scope)
            .bind(&k)
            .execute(&mut *tx)
            .await;
            applied_ops.push(LtmDiffOp {
                op: "DELETE".to_string(),
                slot_key: k,
                old_val: Some(v),
                new_val: None,
                reason: "超出长期记忆容量预算自动精简".to_string(),
            });
        }
    }

    let ops_json = serde_json::to_string(&applied_ops).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT INTO agent_ltm_commits (rev_id, space_id, scope, parent_rev, source_session_id, message, ops_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(&rev_id)
    .bind(space_id)
    .bind(scope)
    .bind(&parent_rev)
    .bind(source_session_id)
    .bind(message)
    .bind(&ops_json)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())?;
    drop(_guard);

    let commit_dto = LtmCommitDto {
        rev_id: rev_id.clone(),
        scope: scope.to_string(),
        parent_rev,
        source_session_id: source_session_id.to_string(),
        message: message.to_string(),
        ops: applied_ops,
        created_at: now,
    };

    // Mirror commit chain to S3 (`agent/spaces/{space_id}/ltm/{scope}/commits.jsonl`)
    let safe_scope = scope.replace(':', "_");
    let s3_commits_key = format!("agent/spaces/{space_id}/ltm/{safe_scope}/commits.jsonl");
    let mut existing_log = state
        .s3
        .get_bytes(&s3_commits_key)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Ok(mut line) = serde_json::to_vec(&commit_dto) {
        line.push(b'\n');
        existing_log.extend_from_slice(&line);
        let _ = state
            .s3
            .put_bytes(&s3_commits_key, "application/x-ndjson", &existing_log)
            .await;
    }

    Ok(Some(commit_dto))
}

/// Extracts a sealed Session (`1 Session = 1 Append-Only STM Segment` per scope) + ambiguity clarifications.
pub async fn extract_sealed_session_to_stm(
    state: &AppState,
    space_id: i64,
    user_id: i64,
    session_id: &str,
) -> Result<(), String> {
    let turns = load_session_turns_from_s3(state, space_id, user_id, session_id).await;
    if turns.is_empty() {
        let _ = sqlx::query("UPDATE agent_sessions SET stm_extracted = 1 WHERE session_id = ?1")
            .bind(session_id)
            .execute(&state.db.write)
            .await;
        return Ok(());
    }

    let mut convo = String::new();
    for t in &turns {
        convo.push_str(&format!("[{}]: {}\n", t.role, t.text));
        if let Some(act) = &t.ui_action_summary {
            convo.push_str(&format!("[用户界面交互]: {act}\n"));
        }
    }

    let extract_sys = r#"你是 KeepDays 数字自宅宠物的「Session -> 短期记忆」提炼器。
请阅读这一整段已结束的对话，输出严格的 JSON（不要 Markdown 代码块）：
{
  "private_summary": "站在当前对话人视角的本场完整摘要（保留情绪、个人偏好、惊喜筹备等细节，50~220字）",
  "shared_summary": "仅当包含适合伴侣双方共享的家庭公共日常事实时填写（如共同饮食、纪念日、家务待办；严禁包含单方吐槽、惊喜计划或个人隐私！没有则填空字符串）",
  "clarifications": ["如果对话中有指代不明或关键事实模糊（如‘老地方’、未说明日期的约定），写出1条温和顺嘴追问提示，没有则空数组"]
}"#;

    let comp = complete_with_failover(
        state,
        space_id,
        ModelRole::Memory,
        extract_sys,
        "",
        &[LlmMessage::User { content: convo }],
        &[],
    )
    .await?;

    let parsed = extract_json_object(&comp.text).unwrap_or_else(|| {
        let fallback_sum: String = turns
            .iter()
            .filter(|t| t.role == "user")
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join("；")
            .chars()
            .take(200)
            .collect();
        json!({
            "private_summary": fallback_sum,
            "shared_summary": "",
            "clarifications": []
        })
    });

    let now = now_ms();
    let priv_sum = parsed
        .get("private_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let shared_sum = parsed
        .get("shared_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if !priv_sum.is_empty() {
        insert_stm_segment(
            state,
            space_id,
            &user_scope(user_id),
            session_id,
            user_id,
            priv_sum,
            now,
        )
        .await?;
    }
    if !shared_sum.is_empty() {
        insert_stm_segment(
            state,
            space_id,
            "shared",
            session_id,
            user_id,
            shared_sum,
            now,
        )
        .await?;
    }

    if let Some(arr) = parsed.get("clarifications").and_then(|v| v.as_array()) {
        for q in arr.iter().take(2) {
            if let Some(qs) = q.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                let cid = state.snowflake.next_id();
                let _ = sqlx::query(
                    "INSERT INTO agent_clarifications
                     (id, space_id, user_id, source_session_id, question_hint, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'PENDING', ?6)",
                )
                .bind(cid)
                .bind(space_id)
                .bind(user_id)
                .bind(session_id)
                .bind(qs)
                .bind(now)
                .execute(&state.db.write)
                .await;
            }
        }
    }

    let _ = sqlx::query("UPDATE agent_sessions SET stm_extracted = 1 WHERE session_id = ?1")
        .bind(session_id)
        .execute(&state.db.write)
        .await;

    Ok(())
}

async fn insert_stm_segment(
    state: &AppState,
    space_id: i64,
    scope: &str,
    source_session_id: &str,
    speaker_user_id: i64,
    summary_text: &str,
    now: i64,
) -> Result<(), String> {
    let seg_id = state.snowflake.next_id();
    let tokens = segment_for_bm25(summary_text);
    let s3_key = format!("agent/spaces/{space_id}/stm/{seg_id}.json");

    let doc = json!({
        "id": seg_id.to_string(),
        "space_id": space_id.to_string(),
        "scope": scope,
        "source_session_id": source_session_id,
        "speaker_user_id": speaker_user_id.to_string(),
        "summary_text": summary_text,
        "created_at": now,
    });
    state
        .s3
        .put_bytes(
            &s3_key,
            "application/json",
            &serde_json::to_vec(&doc).unwrap_or_default(),
        )
        .await?;

    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO agent_stm_segments
         (id, space_id, scope, source_session_id, speaker_user_id, summary_text, keywords_seg, promoted_to_ltm, s3_key, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9)",
    )
    .bind(seg_id)
    .bind(space_id)
    .bind(scope)
    .bind(source_session_id)
    .bind(speaker_user_id)
    .bind(summary_text)
    .bind(&tokens)
    .bind(&s3_key)
    .bind(now)
    .execute(&state.db.write)
    .await
    .map_err(|e| e.to_string())?;

    if inserted.rows_affected() > 0 {
        let space_scope = format!("{space_id}:{scope}");
        let _ = sqlx::query(
            "INSERT INTO agent_stm_fts (segment_id, space_scope, tokens, summary)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(seg_id.to_string())
        .bind(&space_scope)
        .bind(&tokens)
        .bind(summary_text)
        .execute(&state.db.write)
        .await;
    }

    Ok(())
}

/// Promotes unpromoted STM segments into Git-Style LTM (`Private -> Private`, `Shared -> Shared` ONLY!).
pub async fn promote_stm_to_ltm(state: &AppState) {
    let rows = match sqlx::query(
        "SELECT id, space_id, scope, source_session_id, speaker_user_id, summary_text
         FROM agent_stm_segments
         WHERE promoted_to_ltm = 0 AND promote_attempts < 5 AND archived_at IS NULL
         ORDER BY created_at ASC
         LIMIT 8",
    )
    .fetch_all(&state.db.read)
    .await
    {
        Ok(r) => r,
        Err(_) => return,
    };

    for r in rows {
        let seg_id: i64 = r.get("id");
        let space_id: i64 = r.get("space_id");
        let scope: String = r.get("scope");
        let session_id: String = r.get("source_session_id");
        let speaker_uid: i64 = r.get("speaker_user_id");
        let stm_text: String = r.get("summary_text");

        let cur_head = sqlx::query(
            "SELECT slot_key, slot_value FROM agent_ltm_head
             WHERE space_id = ?1 AND scope = ?2",
        )
        .bind(space_id)
        .bind(&scope)
        .fetch_all(&state.db.read)
        .await
        .unwrap_or_default();

        let head_str = cur_head
            .iter()
            .map(|hr| {
                format!(
                    "{} = {}",
                    hr.get::<String, _>("slot_key"),
                    hr.get::<String, _>("slot_value")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let sys = r#"你是长期记忆 Git 式变更引擎。对比【当前长期记忆槽位】与【新增短期记忆片段】：
1. 仅提炼值得跨周保留的长期稳定画像（如饮食偏好/忌口、纪念日、作息习惯、性格细节、重要约定）。临时琐事不要写入长期记忆。
2. 如果是状态演进（如以前爱喝冰美式，现在养胃改喝燕麦拿铁），输出 op="REPLACE" 更新该 slot_key。
3. 如果跟核心锚点（如纪念日日期、严重过敏原）发生直接矛盾且无法确定真伪，不要修改槽位，将其放入 contradictions 数组等待向主人求证！
严格输出 JSON：
{
  "commit_message": "简短变更说明",
  "ops": [
    { "op": "ADD|REPLACE|DELETE", "slot_key": "分类.键名", "new_val": "精炼值", "reason": "变更原因" }
  ],
  "contradictions": ["如有核心事实矛盾，写一句温和求证问题"]
}"#;

        let prompt = format!(
            "【作用域】: {scope}\n【当前长期记忆槽位】:\n{}\n\n【新增短期记忆片段】:\n{stm_text}",
            if head_str.is_empty() {
                "(暂无)"
            } else {
                &head_str
            }
        );

        let mut promoted_ok = false;
        let mut skipped_local_fallback = false;

        if let Ok(comp) = complete_with_failover(
            state,
            space_id,
            ModelRole::Memory,
            sys,
            "",
            &[LlmMessage::User { content: prompt }],
            &[],
        )
        .await
        {
            if comp.provider_used == "builtin:local" {
                skipped_local_fallback = true;
            } else if let Some(obj) = extract_json_object(&comp.text) {
                let msg = obj
                    .get("commit_message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("从短期记忆自动演进")
                    .to_string();
                let ops: Vec<LtmDiffOp> = obj
                    .get("ops")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                if apply_ltm_git_commit(state, space_id, &scope, &session_id, &msg, ops)
                    .await
                    .is_ok()
                {
                    promoted_ok = true;
                }

                if let Some(contras) = obj.get("contradictions").and_then(|v| v.as_array()) {
                    for c in contras.iter().take(1) {
                        if let Some(q) = c.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                            let _ = sqlx::query(
                                "INSERT INTO agent_clarifications
                                 (id, space_id, user_id, source_session_id, question_hint, status, created_at)
                                 VALUES (?1, ?2, ?3, ?4, ?5, 'PENDING', ?6)",
                            )
                            .bind(state.snowflake.next_id())
                            .bind(space_id)
                            .bind(speaker_uid)
                            .bind(&session_id)
                            .bind(q)
                            .bind(now_ms())
                            .execute(&state.db.write)
                            .await;
                        }
                    }
                }
            }
        }

        if skipped_local_fallback {
            // Keep `promoted_to_ltm = 0` untouched until an external LLM provider is configured!
            continue;
        }

        if promoted_ok {
            let _ = sqlx::query("UPDATE agent_stm_segments SET promoted_to_ltm = 1 WHERE id = ?1")
                .bind(seg_id)
                .execute(&state.db.write)
                .await;
        } else {
            let _ = sqlx::query(
                "UPDATE agent_stm_segments SET promote_attempts = promote_attempts + 1 WHERE id = ?1",
            )
            .bind(seg_id)
            .execute(&state.db.write)
            .await;
        }
    }
}

/// Safe 7-Day Short-Term Memory GC: ONLY removes segments where `age > 7d AND promoted_to_ltm = 1`!
pub async fn gc_expired_stm_segments(state: &AppState) {
    let cutoff = now_ms() - STM_RETENTION_MS;
    let rows = match sqlx::query(
        "SELECT id, space_id, s3_key FROM agent_stm_segments
         WHERE promoted_to_ltm = 1 AND archived_at IS NULL AND created_at < ?1
         LIMIT 32",
    )
    .bind(cutoff)
    .fetch_all(&state.db.read)
    .await
    {
        Ok(r) => r,
        Err(_) => return,
    };

    let now = now_ms();
    for r in rows {
        let id: i64 = r.get("id");
        let space_id: i64 = r.get("space_id");
        let old_key: String = r.get("s3_key");

        // Archive S3 object under `stm_archive/` before removing from hot FTS5 index
        if let Ok(Some(bytes)) = state.s3.get_bytes(&old_key).await {
            let archive_key = format!("agent/spaces/{space_id}/stm_archive/{id}.json");
            if state
                .s3
                .put_bytes(&archive_key, "application/json", &bytes)
                .await
                .is_ok()
            {
                state.s3.delete_object(&old_key).await;
            }
        }

        let _ = sqlx::query("DELETE FROM agent_stm_fts WHERE segment_id = ?1")
            .bind(id.to_string())
            .execute(&state.db.write)
            .await;
        let _ = sqlx::query("UPDATE agent_stm_segments SET archived_at = ?1 WHERE id = ?2")
            .bind(now)
            .bind(id)
            .execute(&state.db.write)
            .await;
    }
}

pub fn extract_json_object(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if v.is_object() {
            return Some(v);
        }
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end > start {
        serde_json::from_str::<Value>(&trimmed[start..=end])
            .ok()
            .filter(|v| v.is_object())
    } else {
        None
    }
}

/// Renders `agent_ltm_head` for a single scope (`"shared"` or `"user:{uid}"`) into human-editable Markdown.
pub async fn render_scope_ltm_markdown(
    state: &AppState,
    space_id: i64,
    scope: &str,
) -> Result<String, String> {
    let rows = sqlx::query(
        "SELECT slot_key, slot_value
         FROM agent_ltm_head
         WHERE space_id = ?1 AND scope = ?2
         ORDER BY updated_at ASC, slot_key ASC",
    )
    .bind(space_id)
    .bind(scope)
    .fetch_all(&state.db.read)
    .await
    .map_err(|e| e.to_string())?;

    let title = if scope == "shared" {
        "# 家庭公共记忆"
    } else {
        "# 我的专属私密记忆"
    };

    if rows.is_empty() {
        let hint = if scope == "shared" {
            "<!-- 这里记录你们共同的饮食偏好、生活习惯与重要纪念日。直接用 Markdown 列表书写即可，保存后立即同步给陪伴伙伴。 -->"
        } else {
            "<!-- 这里记录仅属于你的私密备忘（如给对方的惊喜计划、个人偏好），物理隔离绝不向另一半透露。 -->"
        };
        return Ok(format!("{title}\n\n{hint}\n"));
    }

    let mut general_lines: Vec<String> = Vec::new();
    let mut sectioned: Vec<(String, Vec<String>)> = Vec::new();

    for r in rows {
        let k: String = r.get("slot_key");
        let v: String = r.get("slot_value");
        let (sec_opt, body) = if let Some(rest) = v.strip_prefix('[') {
            if let Some((sec, tail)) = rest.split_once("] ") {
                if !sec.trim().is_empty() && sec.chars().count() <= 24 {
                    (Some(sec.trim().to_string()), tail.trim().to_string())
                } else {
                    (None, v)
                }
            } else {
                (None, v)
            }
        } else {
            (None, v)
        };

        let bullet = if k.starts_with("md_") {
            format!("- {body}")
        } else {
            format!("- **{k}**: {body}")
        };

        if let Some(sec_name) = sec_opt {
            if let Some(existing) = sectioned.iter_mut().find(|(s, _)| s == &sec_name) {
                existing.1.push(bullet);
            } else {
                sectioned.push((sec_name, vec![bullet]));
            }
        } else {
            general_lines.push(bullet);
        }
    }

    let mut out = format!("{title}\n\n");
    if !general_lines.is_empty() {
        out.push_str(&general_lines.join("\n"));
        out.push('\n');
    }
    for (sec, items) in sectioned {
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(&format!("## {sec}\n"));
        out.push_str(&items.join("\n"));
        out.push('\n');
    }

    Ok(out)
}

fn strip_html_comments(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start + 4..].find("-->") {
            rest = &rest[start + 4 + end + 3..];
        } else {
            return out;
        }
    }
    out.push_str(rest);
    out
}

fn stable_md_key(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.trim().as_bytes());
    let digest = hasher.finalize();
    format!(
        "md_{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

/// Parses user-edited Markdown and commits exact `ADD` / `REPLACE` / `DELETE` diffs into `agent_ltm_head` & `agent_ltm_commits`.
pub async fn sync_scope_ltm_from_markdown(
    state: &AppState,
    space_id: i64,
    scope: &str,
    markdown: &str,
) -> Result<Option<LtmCommitDto>, String> {
    let cleaned = strip_html_comments(markdown);
    let mut current_section: Option<String> = None;
    let mut desired: Vec<(String, String)> = Vec::new();

    for raw_line in cleaned.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line == "---" {
            continue;
        }
        if let Some(h1) = line.strip_prefix("# ") {
            let h = h1.trim();
            if h != "家庭公共记忆"
                && h != "我的专属私密记忆"
                && !h.eq_ignore_ascii_case("shared memory")
                && !h.eq_ignore_ascii_case("private memory")
            {
                current_section = Some(h.to_string());
            }
            continue;
        }
        if let Some(h2) = line
            .strip_prefix("### ")
            .or_else(|| line.strip_prefix("## "))
        {
            let h = h2.trim();
            current_section = (!h.is_empty()).then(|| h.to_string());
            continue;
        }

        let item = line
            .strip_prefix("- [ ] ")
            .or_else(|| line.strip_prefix("- [x] "))
            .or_else(|| line.strip_prefix("- "))
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
            .unwrap_or(line)
            .trim();
        if item.is_empty() {
            continue;
        }

        // Parse `- **slot_key**: value` or `- **slot_key**：value`
        let (slot_key, raw_val) = if let Some(after_bold) = item.strip_prefix("**") {
            if let Some((k_cand, rest)) = after_bold.split_once("**") {
                let val_part = rest
                    .trim_start()
                    .strip_prefix(':')
                    .or_else(|| rest.trim_start().strip_prefix('：'))
                    .map(str::trim);
                let clean_k = k_cand.trim();
                if let Some(v) = val_part.filter(|v| !v.is_empty() && !clean_k.is_empty()) {
                    (clean_k.to_string(), v.to_string())
                } else {
                    let full_v = match &current_section {
                        Some(sec) => format!("[{sec}] {item}"),
                        None => item.to_string(),
                    };
                    (stable_md_key(&full_v), item.to_string())
                }
            } else {
                let full_v = match &current_section {
                    Some(sec) => format!("[{sec}] {item}"),
                    None => item.to_string(),
                };
                (stable_md_key(&full_v), item.to_string())
            }
        } else {
            let full_v = match &current_section {
                Some(sec) => format!("[{sec}] {item}"),
                None => item.to_string(),
            };
            (stable_md_key(&full_v), item.to_string())
        };

        let final_val = match &current_section {
            Some(sec) if !raw_val.starts_with('[') => format!("[{sec}] {raw_val}"),
            _ => raw_val,
        };

        if let Some(existing) = desired.iter_mut().find(|(k, _)| k == &slot_key) {
            existing.1 = final_val;
        } else {
            desired.push((slot_key, final_val));
        }
    }

    let existing_rows = sqlx::query(
        "SELECT slot_key, slot_value FROM agent_ltm_head WHERE space_id = ?1 AND scope = ?2",
    )
    .bind(space_id)
    .bind(scope)
    .fetch_all(&state.db.read)
    .await
    .map_err(|e| e.to_string())?;

    let mut existing_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::with_capacity(existing_rows.len());
    for r in existing_rows {
        existing_map.insert(r.get("slot_key"), r.get("slot_value"));
    }

    let desired_keys: std::collections::HashSet<&str> =
        desired.iter().map(|(k, _)| k.as_str()).collect();

    let mut ops: Vec<LtmDiffOp> = Vec::new();
    for (old_k, old_v) in &existing_map {
        if !desired_keys.contains(old_k.as_str()) {
            ops.push(LtmDiffOp {
                op: "DELETE".to_string(),
                slot_key: old_k.clone(),
                old_val: Some(old_v.clone()),
                new_val: None,
                reason: "manual markdown edit".to_string(),
            });
        }
    }

    for (new_k, new_v) in desired {
        match existing_map.get(&new_k) {
            None => ops.push(LtmDiffOp {
                op: "ADD".to_string(),
                slot_key: new_k,
                old_val: None,
                new_val: Some(new_v),
                reason: "manual markdown edit".to_string(),
            }),
            Some(prev_v) if prev_v != &new_v => ops.push(LtmDiffOp {
                op: "REPLACE".to_string(),
                slot_key: new_k,
                old_val: Some(prev_v.clone()),
                new_val: Some(new_v),
                reason: "manual markdown edit".to_string(),
            }),
            _ => {}
        }
    }

    if ops.is_empty() {
        return Ok(None);
    }

    apply_ltm_git_commit(
        state,
        space_id,
        scope,
        "manual_md_edit",
        "手动编辑记忆 Markdown",
        ops,
    )
    .await
}
