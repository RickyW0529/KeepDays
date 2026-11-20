-- ============================================================================
-- KeepDays 0002: Pet Agent Kernel (Modified grok-build Runtime)
-- 1. S3-backed Per-Speaker Isolated Sessions (No hard turn cutoff; idle seal)
-- 2. Append-Only 7-Day Short-Term Memory (FTS5 BM25 + Temporal Decay + MMR)
-- 3. Ambiguity Piggyback Clarification Queue
-- 4. Git-Style Long-Term Memory (HEAD Slots + Parent-Linked Diff Commits)
-- 5. Multi-Provider LLM Router & MCP Server Registry
-- ============================================================================

CREATE TABLE IF NOT EXISTS agent_sessions (
    session_id      TEXT PRIMARY KEY NOT NULL,
    space_id        INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title           TEXT NOT NULL DEFAULT '与伙伴的私语',
    status          TEXT NOT NULL DEFAULT 'ACTIVE', -- 'ACTIVE' | 'SEALED'
    turn_count      INTEGER NOT NULL DEFAULT 0,
    s3_key          TEXT NOT NULL,
    rolling_summary TEXT NOT NULL DEFAULT '',
    stm_extracted   INTEGER NOT NULL DEFAULT 0,
    last_active_at  INTEGER NOT NULL,
    sealed_at       INTEGER,
    created_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_user
    ON agent_sessions(space_id, user_id, last_active_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_seal
    ON agent_sessions(status, last_active_at);

-- L1: Append-Only 7-Day Short-Term Memory Log (1 Sealed Session -> 1 Segment per scope)
CREATE TABLE IF NOT EXISTS agent_stm_segments (
    id                INTEGER PRIMARY KEY NOT NULL,
    space_id          INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    scope             TEXT NOT NULL, -- 'shared' | 'user:{user_id}'
    source_session_id TEXT NOT NULL,
    speaker_user_id   INTEGER NOT NULL,
    summary_text      TEXT NOT NULL,
    keywords_seg      TEXT NOT NULL,
    promoted_to_ltm   INTEGER NOT NULL DEFAULT 0,
    s3_key            TEXT NOT NULL,
    created_at        INTEGER NOT NULL,
    archived_at       INTEGER,
    UNIQUE(source_session_id, scope)
);
CREATE INDEX IF NOT EXISTS idx_agent_stm_active
    ON agent_stm_segments(space_id, scope, archived_at, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_stm_gc
    ON agent_stm_segments(promoted_to_ltm, archived_at, created_at);

-- Native SQLite FTS5 Index for Okapi BM25 Recall over Segmented Chinese + Alphanumeric Tokens
CREATE VIRTUAL TABLE IF NOT EXISTS agent_stm_fts USING fts5(
    segment_id UNINDEXED,
    space_scope UNINDEXED,
    tokens,
    summary
);

-- Ambiguous Short-Term Memory Clarification Queue (Piggyback gentle inquiry)
CREATE TABLE IF NOT EXISTS agent_clarifications (
    id                INTEGER PRIMARY KEY NOT NULL,
    space_id          INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    user_id           INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source_session_id TEXT NOT NULL,
    question_hint     TEXT NOT NULL,
    context_quote     TEXT NOT NULL DEFAULT '',
    status            TEXT NOT NULL DEFAULT 'PENDING', -- 'PENDING' | 'ASKED' | 'RESOLVED'
    asked_at          INTEGER,
    resolved_at       INTEGER,
    created_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_clarify_user
    ON agent_clarifications(space_id, user_id, status, created_at DESC);

-- L2 HEAD: Long-Term Memory Active Keyed Slots (Injected right after System Prompt)
CREATE TABLE IF NOT EXISTS agent_ltm_head (
    space_id    INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    scope       TEXT NOT NULL, -- 'shared' | 'user:{user_id}'
    slot_key    TEXT NOT NULL, -- e.g. 'diet.coffee', 'anniversary.date'
    slot_value  TEXT NOT NULL,
    current_rev TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (space_id, scope, slot_key)
);

-- L2 History: Git-Style Append-Only Commit & Diff Log
CREATE TABLE IF NOT EXISTS agent_ltm_commits (
    rev_id            TEXT PRIMARY KEY NOT NULL,
    space_id          INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    scope             TEXT NOT NULL, -- 'shared' | 'user:{user_id}'
    parent_rev        TEXT,
    source_session_id TEXT NOT NULL,
    message           TEXT NOT NULL,
    ops_json          TEXT NOT NULL, -- JSON array of {op, slot_key, old_val, new_val, reason}
    created_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_ltm_commits_scope
    ON agent_ltm_commits(space_id, scope, created_at DESC);

-- Multi-Provider LLM Router Registry (Space-scoped or Global space_id=0)
CREATE TABLE IF NOT EXISTS agent_providers (
    id                TEXT PRIMARY KEY NOT NULL,
    space_id          INTEGER NOT NULL DEFAULT 0,
    name              TEXT NOT NULL,
    protocol          TEXT NOT NULL, -- 'openai_compat' | 'anthropic' | 'gemini'
    base_url          TEXT NOT NULL,
    api_key           TEXT NOT NULL,
    chat_model        TEXT NOT NULL,
    memory_model      TEXT NOT NULL DEFAULT '',
    is_chat_primary   INTEGER NOT NULL DEFAULT 0,
    is_memory_primary INTEGER NOT NULL DEFAULT 0,
    enabled           INTEGER NOT NULL DEFAULT 1,
    priority          INTEGER NOT NULL DEFAULT 100,
    updated_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_agent_providers_space
    ON agent_providers(space_id, enabled, priority ASC);

-- MCP (Model Context Protocol) External Tool Server Registry
CREATE TABLE IF NOT EXISTS agent_mcp_servers (
    id           TEXT PRIMARY KEY NOT NULL,
    space_id     INTEGER NOT NULL DEFAULT 0,
    name         TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    transport    TEXT NOT NULL, -- 'http' | 'stdio'
    endpoint     TEXT NOT NULL, -- URL for http, command for stdio
    args_json    TEXT NOT NULL DEFAULT '[]',
    headers_json TEXT NOT NULL DEFAULT '{}',
    enabled      INTEGER NOT NULL DEFAULT 1,
    updated_at   INTEGER NOT NULL
);
