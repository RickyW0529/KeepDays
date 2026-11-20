-- KeepDays Snowflake Dimensional + Hybrid Document Schema (SQLite 3.45+)
PRAGMA foreign_keys = ON;

-- ============================================================================
-- 1. 雪花维度模型字典中心 (以后新增任何媒体格式、空间模块、事件类型仅需 INSERT 此表)
-- ============================================================================

CREATE TABLE IF NOT EXISTS dim_domains (
    id INTEGER PRIMARY KEY NOT NULL,
    code TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS dim_items (
    id INTEGER PRIMARY KEY NOT NULL,
    domain_id INTEGER NOT NULL REFERENCES dim_domains(id),
    code TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_active INTEGER NOT NULL DEFAULT 1,
    config_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_dim_items_domain ON dim_items(domain_id, is_active, sort_order);

-- 预置二级维度域
INSERT OR IGNORE INTO dim_domains (id, code, name) VALUES
    (10, 'SPACE_TYPE',     '空间类型域'),
    (20, 'MEMBER_ROLE',    '空间成员角色域'),
    (30, 'COMMON_STATUS',  '通用生命周期状态域'),
    (40, 'ASSET_KIND',     'S3媒体资产类型域'),
    (60, 'SPACE_MODULE',   '空间交互模块类型域'),
    (70, 'TIMELINE_EVENT', '类Twitter时间线事件域');

-- 预置一级维度字典项 (携带动态规则 config_json)
INSERT OR IGNORE INTO dim_items (id, domain_id, code, label, sort_order, config_json) VALUES
    -- 10: 空间类型
    (1001, 10, 'PERSONAL', '个人独处空间', 1, '{"allow_invite":false,"has_pet":false}'),
    (1002, 10, 'SHARED',   '多人共享空间', 2, '{"allow_invite":true,"has_pet":true}'),

    -- 20: 成员角色
    (2001, 20, 'OWNER',    '空间主理人',   1, '{}'),
    (2002, 20, 'MEMBER',   '共建成员',     2, '{}'),

    -- 30: 状态码
    (3001, 30, 'PENDING',  '待处理/上传中', 1, '{}'),
    (3002, 30, 'READY',    '已就绪/已同意', 2, '{}'),
    (3003, 30, 'REJECTED', '已拒绝',       3, '{}'),
    (3004, 30, 'EXPIRED',  '已过期',       4, '{}'),

    -- 40: S3 媒体资产种类 (目录前缀、体积上限与允许 MIME 均由表配置驱动！)
    (4001, 40, 'PHOTO',    '照片墙原图',   1, '{"s3_dir":"photos","max_bytes":15728640,"mimes":["image/webp","image/jpeg","image/png"]}'),
    (4002, 40, 'LIVE_MP4', '实况微动视频', 2, '{"s3_dir":"live","max_bytes":10485760,"mimes":["video/mp4","video/quicktime","video/webm"]}'),
    (4003, 40, 'VOICE',    '朋友圈声波条', 3, '{"s3_dir":"voice","max_bytes":3145728,"mimes":["audio/webm","audio/mp4","audio/ogg","audio/mpeg"]}'),
    (4004, 40, 'AVATAR',   '用户高清头像', 4, '{"s3_dir":"avatars","max_bytes":5242880,"mimes":["image/webp","image/jpeg","image/png"]}'),
    (4005, 40, 'BANNER',   '个人名片横幅', 5, '{"s3_dir":"banners","max_bytes":8388608,"mimes":["image/webp","image/jpeg","image/png"]}'),
    (4006, 40, 'DISH_IMG', '家庭菜谱配图', 6, '{"s3_dir":"dishes","max_bytes":8388608,"mimes":["image/webp","image/jpeg","image/png"]}'),

    -- 60: 空间交互模块 (右下角冰箱 & 共享宠物)
    (6001, 60, 'FRIDGE',   '冰箱门最近5便签', 1, '{"max_slips":5}'),
    (6002, 60, 'MENU',     '家庭小菜单',     2, '{}'),
    (6003, 60, 'BOARD',    '留言与待办板',   3, '{}'),
    (6004, 60, 'PET',      '共享空间宠物',   4, '{"shared_only":true}'),

    -- 70: 类 Twitter 写扩散时间线事件类型
    (7001, 70, 'PHOTO_POSTED',    '发布新照片',       1, '{}'),
    (7002, 70, 'PHOTO_COMMENTED', '照片朋友圈新留言', 2, '{}'),
    (7003, 70, 'PHOTO_LIKED',     '点赞照片',         3, '{}'),
    (7004, 70, 'MENU_PINNED',     '贴上今日菜单便签', 4, '{}'),
    (7005, 70, 'BOARD_UPDATED',   '更新留言/待办板',  5, '{}'),
    (7006, 70, 'PROFILE_UPDATED', '更新个人名片动态', 6, '{}'),
    (7007, 70, 'PET_MOMENT',      '共享宠物动态',     7, '{}'),
    (7008, 70, 'MEMBER_JOINED',   '新成员入住空间',   8, '{}');

-- ============================================================================
-- 2. 核心实体与事实表 (全 64-bit Snowflake INTEGER 主键，直通 SQLite 物理 rowid)
-- ============================================================================

-- 用户与领英式模块化个人档案表
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY NOT NULL,
    username TEXT NOT NULL UNIQUE,
    doorplate_no TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    profile_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_users_doorplate ON users(doorplate_no);

-- 空间租户主表
CREATE TABLE IF NOT EXISTS spaces (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    space_type_id INTEGER NOT NULL REFERENCES dim_items(id),
    owner_id INTEGER NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL
);

-- 空间成员与权限隔离表
CREATE TABLE IF NOT EXISTS space_members (
    space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id INTEGER NOT NULL REFERENCES dim_items(id),
    alias_name TEXT,
    joined_at INTEGER NOT NULL,
    PRIMARY KEY (space_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_members_user ON space_members(user_id);

-- 空间邀请与同意握手表
CREATE TABLE IF NOT EXISTS space_invitations (
    id INTEGER PRIMARY KEY NOT NULL,
    space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    inviter_id INTEGER NOT NULL REFERENCES users(id),
    invitee_id INTEGER REFERENCES users(id),
    invite_code TEXT NOT NULL UNIQUE,
    status_id INTEGER NOT NULL REFERENCES dim_items(id),
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_invites_invitee ON space_invitations(invitee_id, status_id);

-- S3 媒体资产解耦与引用计数表
CREATE TABLE IF NOT EXISTS media_assets (
    id INTEGER PRIMARY KEY NOT NULL,
    uploader_id INTEGER NOT NULL REFERENCES users(id),
    kind_id INTEGER NOT NULL REFERENCES dim_items(id),
    status_id INTEGER NOT NULL REFERENCES dim_items(id),
    mime_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    has_thumb INTEGER NOT NULL DEFAULT 0,
    ref_count INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_assets_gc ON media_assets(status_id, ref_count, created_at);

-- 全屏照片墙主表 (分离 post_id 与底层 asset_id，支撑零拷贝跨空间冲洗)
CREATE TABLE IF NOT EXISTS photos (
    id INTEGER PRIMARY KEY NOT NULL,
    space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    asset_id INTEGER NOT NULL REFERENCES media_assets(id),
    taken_at INTEGER NOT NULL,
    post_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_photos_wall ON photos(space_id, taken_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_photos_asset ON photos(asset_id);

-- 空间交互模块单文档表 (冰箱最近5便签 / 家庭菜单 / 留言待办板 / 共享宠物)
CREATE TABLE IF NOT EXISTS space_modules (
    space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    module_type_id INTEGER NOT NULL REFERENCES dim_items(id),
    version INTEGER NOT NULL DEFAULT 1,
    data_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (space_id, module_type_id)
);

-- 类 Twitter 写扩散时间线持久化兜底事实表
CREATE TABLE IF NOT EXISTS timeline_events (
    id INTEGER PRIMARY KEY NOT NULL,
    space_id INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    actor_id INTEGER NOT NULL REFERENCES users(id),
    event_type_id INTEGER NOT NULL REFERENCES dim_items(id),
    payload_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_timeline_cursor ON timeline_events(space_id, id DESC);

-- 持久化刷新凭证会话表（零内存开销、重启不掉线、支持 10 秒并发轮换宽限窗口）
CREATE TABLE IF NOT EXISTS refresh_sessions (
    token_hash TEXT PRIMARY KEY NOT NULL,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    grace_access_uid INTEGER,
    grace_until INTEGER,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_refresh_sessions_exp ON refresh_sessions(expires_at);
