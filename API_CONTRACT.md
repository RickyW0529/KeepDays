# KeepDays 前端对接 API 契约手册 (`v1`)

- **Base URL**: `/api/v1`
- **认证方式**:
  - 普通请求 Header: `Authorization: Bearer <access_token>`
  - 原生浏览器 `EventSource` SSE 连接: `?token=<access_token>`
  - 会话自动续期: `POST /api/v1/auth/refresh` (读取 `HttpOnly` Cookie `kd_refresh_token` 并进行单次轮换)
- **64-bit Snowflake ID 精度约定**:
  - 后端与 SQLite 内部全部采用 64 位 `i64` 整型主键 (`INTEGER PRIMARY KEY`)；
  - 所有 JSON 响应中，ID 字段统一序列化为 **字符串**（如 `"7301928374651904001"`），避免前端 JS `Number` 超过 $2^{53}-1$ 丢失精度。
- **零二次请求媒体注入 (`asset_urls` & `users`)**:
  - 凡是携带媒体或用户引用的读取接口，均随响应附带 `users: Record<uid, UserSummaryDto>` 与 `asset_urls: Record<asset_id, { full_url, thumb_url }>`（Redis 25 分钟固定窗口预签名缓存，100% 命中浏览器本地磁盘缓存）。

---

## 1. 系统状态与雪花维度字典 (`/api/v1/system`)

| 方法 | 路径 | 鉴权 | 说明 |
| :--- | :--- | :---: | :--- |
| `GET` | `/system/health` | 否 | 健康检查 + 当前北京时间 (`Asia/Shanghai`) 昼夜色温状态 (`DAYLIGHT` / `DUSK` / `NIGHT`, `fridge_nightlight_on`) |
| `GET` | `/system/dict` | 否 | 获取全部雪花维度域 (`dim_domains`) 与字典项 (`dim_items` 含媒体上传限制配置) |
| `POST` | `/system/dict/items` | 是 | 动态新增或修改维度字典项（如新增媒体格式或新事件码），零改表热生效 |

---

## 2. 认证与门牌号账号体系 (`/api/v1/auth`)

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `POST` | `/auth/register` | `{ username, password, nickname? }` ➔ Argon2id 加密落库、自动分配唯一 `#doorplate_no` 门牌号、自动初始化独立「个人空间」，返回 `{ access_token, user, default_personal_space_id }` |
| `POST` | `/auth/login` | `{ username, password }` ➔ 登录签发 Token 对 |
| `POST` | `/auth/refresh` | 轮换 HttpOnly Refresh Cookie 获取新 `access_token` |
| `POST` | `/auth/logout` | 吊销 Refresh Token 并清除 Cookie |

---

## 3. 领英式模块化个人档案 (`/api/v1/profile`)

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `GET` | `/profile/me` | 获取当前用户的模块化档案 `profile_json` 及预签名头像/横幅 `asset_urls` |
| `PUT` | `/profile/me` | 更新 `{ nickname, signature, avatar_asset_id?, banner_asset_id?, sections: [...] }`（自动维护 S3 引用计数并向共享空间推送 Timeline） |
| `GET` | `/profile/:user_id` | 查看同空间成员或受邀对象的个人名片 |

---

## 4. 多空间租户、门牌号邀请与同屋微感知 (`/api/v1/spaces`)

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `GET` | `/spaces` | 获取我拥有的「个人空间」与已加入的所有「共享空间」列表 |
| `POST` | `/spaces` | `{ name, alias_name? }` 新建一个多人共享空间（自动挂载未领养状态的宠物模块） |
| `POST` | `/spaces/:space_id/invitations` | `{ target_doorplate_no?: "#89757", ttl_minutes?: 1440 }` 发起定向门牌号邀请或生成一次性口令 |
| `GET` | `/invitations/pending` | 查看发给我的待处理空间邀请函 |
| `POST` | `/invitations/:invite_code/respond` | `{ accept: true \| false, alias_name? }` 同意或拒绝入住共享空间 |
| `PUT` | `/spaces/:space_id/presence` | `{ focus: "FRIDGE" \| "PHOTO" \| "IDLE", photo_id? }` 上报同屋微感知焦点（点亮对方微缩小冰箱门缝暖光 & 照片呼吸微光框） |
| `GET` | `/spaces/:space_id/presence` | 获取当前空间同屋在线焦点状态 |
| `GET` | `/spaces/:space_id/events?token=<jwt>` | **SSE 实时流**：推送 `CONNECTED`、`TIMELINE_NEW`、`PRESENCE_SYNC`、`FRIDGE_UPDATED`、`PET_UPDATED` |

---

## 5. S3 / Cloudflare R2 媒体直传与解耦 (`/api/v1/media`)

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `POST` | `/media/presign-upload` | `{ kind_id: 4001..4006, mime_type, size_bytes, has_thumb }` ➔ 按雪花字典规则校验并返回 `{ asset_id, put_url, thumb_put_url }` 供浏览器直传 S3/R2 |
| `POST` | `/media/:asset_id/confirm` | 前端直传完成后可选调用确认就绪（发帖绑定时也会自动置为就绪并 `ref_count + 1`） |
| `GET` | `/media/hydrate?ids=id1,id2` | 按需批量换取缓存的预签名访问 URL |

---

## 6. 满屏照片墙、朋友圈声波留言与跨空间冲洗 (`/api/v1/spaces/:space_id/photos`)

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `GET` | `.../photos?before_taken_at=&before_id=&limit=36` | 时间流游标分页加载满屏照片墙（单次请求返回完整朋友圈 `post`、`users`、`asset_urls`） |
| `POST` | `.../photos` | `{ asset_id, live_asset_id?, width, height, taken_at?, caption, exif }` 发布新照片（支持 Live Photo；共享空间自动奖励宠物 +15 爪印币） |
| `GET` | `.../photos/:photo_id` | 获取单张照片完整详情 |
| `POST` | `.../photos/:photo_id/like` | 切换微信朋友圈式爱心点赞 |
| `POST` | `.../photos/:photo_id/comments` | `{ reply_to_comment_id?, text?, audio_asset_id?, duration_sec?, waveform?: number[] }` SQLite `json_insert` 原子追加文字或 15 秒声波语音留言 |
| `POST` | `.../photos/:photo_id/project` | `{ target_space_id, override_caption? }` **跨空间零拷贝一键冲洗**（复用底层 S3 `asset_id`，`ref_count + 1`） |
| `DELETE` | `.../photos/:photo_id` | 删除照片帖（`ref_count - 1`，仅当引用归零才异步清理 S3 物理文件） |

---

## 7. 右下角 UI 冰箱三级联动 (`/api/v1/spaces/:space_id/fridge`)

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `GET` | `.../fridge/bundle` | **一键加载整扇冰箱门**：包含 `fridge.recent_slips`（最近5张菜单便签）、`menu`（家庭菜单）、`board`（留言待办板，未到北京时间 `unlock_at` 的毛玻璃便签服务端自动抹除正文防 F12 偷看！） |
| `PUT` | `.../fridge/menu` | `{ expected_version, categories, dishes }` 乐观锁保存家庭菜单 |
| `POST` | `.../fridge/slips` | `{ dish_ids, custom_dishes?, date_label?, note }` 勾选确定今日菜单贴上冰箱门（严格截断只保留最近 5 个，奖励宠物 +20 爪印币） |
| `PATCH` | `.../fridge/slips/:slip_id` | `{ done: boolean }` 标记今日菜单便签 `[已完食]` |
| `POST` | `.../fridge/slips/:slip_id/to-grocery` | **一键转买菜待办**：提取该便签菜品关联食材，自动去重推入 `board.todos` |
| `PUT` | `.../fridge/board` | `{ expected_version, notes, todos }` 乐观锁保存留言/待办板（支持 `unlock_at` 毛玻璃定时显影便签） |
| `POST` | `.../fridge/menu/project/:target_space_id` | `{ dish_id }` 将个人空间练熟的私房菜谱一键零拷贝投递至共享空间菜单 |

---

## 8. 共享空间专属宠物引擎 (`/api/v1/spaces/:space_id/pet`)

*(注：仅 `space_type_id = 1002` 共享空间可用；个人空间请求返回 400)*

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `GET` | `.../pet` | 触发惰性时间求值（Lazy Tick），返回宠物实时状态（含沿墙漫步目标 `focus_photo_id`、嘴叼沉底老照片 `carrying_old_photo_id`）、杂货铺图鉴与纪念明信片 |
| `POST` | `.../pet/adopt` | `{ name, species: "BLACK_CAT" \| "SHIBA" \| "OTTER" \| "BUNNY" }` 共同领养仪式 |
| `POST` | `.../pet/interact` | `{ action: "STROKE" \| "PLAY" \| "FEED" \| "OPEN_DUO_CAN" \| "FINISH_DUO_CAN", item_id? }` 互动（5 分钟内双人先后互动自动触发 `sweet_resonance_triggered = true` 双倍羁绊！） |
| `POST` | `.../pet/shop/buy` | `{ item_id, quantity? }` 使用生活赚取的爪印币兑换冻干、双拼罐、瓦楞纸箱窝或红围巾 |
| `POST` | `.../pet/equip` | `{ bed_style?, accessory? }` 更换冰箱顶窝垫或配饰 |
| `POST` | `.../pet/adventure` | 送精力充沛的宠物出门散步探险（归来自动抽取空间老照片合成「故地重游纪念明信片」） |
| `POST` | `.../pet/paw-pick-menu` | **实用生活技能「伸爪帮选菜」**：从家庭菜单智能拍选一荤一素一汤组合 |

---

## 9. 类 Twitter 写扩散无感时间流 (`/api/v1/spaces/:space_id/timeline`)

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `GET` | `.../timeline?before_id=&limit=24` | 0.2ms 优先从 Redis/内存热队列切片返回预组装动态流（深分页无缝回源 SQLite Snowflake 索引），供前端实现静默预解码与 `[ ↑ 有新动态 ]` 零抖动插入 |
