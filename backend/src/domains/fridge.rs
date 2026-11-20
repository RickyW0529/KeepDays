use axum::{
    extract::{Path, State},
    routing::{get, patch, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::{HashMap, HashSet};

use crate::{
    domains::{
        media::{
            bind_owned_asset_in_tx, clone_existing_asset_ref_in_tx, release_asset_ref_by,
            resolve_asset_urls,
        },
        profile::{load_users_map, UserSummaryDto},
        timeline::record_timeline_event,
    },
    error::{AppError, AppResult},
    infra::{cache::CachedAssetUrls, dict::dim},
    middleware::guards::SpaceGuard,
    snowflake::{i64_str, opt_i64_str, vec_i64_str},
    state::{AppState, PetLifeEvent, PetLifeTrigger},
    time_bj::{now_beijing, now_ms},
};

const MAX_RECENT_FRIDGE_SLIPS: usize = 5;

pub fn space_router() -> Router<AppState> {
    Router::new()
        .route("/bundle", get(get_fridge_bundle))
        .route("/menu", put(update_menu))
        .route("/slips", post(pin_today_menu_slip))
        .route("/slips/:slip_id", patch(toggle_slip_done))
        .route("/slips/:slip_id/to-grocery", post(convert_slip_to_grocery))
        .route("/board", put(update_board))
        .route("/menu/project/:target_space_id", post(project_dish_to_space))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FridgeDoc {
    #[serde(default)]
    pub recent_slips: Vec<FridgeSlip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FridgeSlip {
    #[serde(with = "i64_str")]
    pub id: i64,
    pub date_label: String,
    #[serde(with = "i64_str")]
    pub by_uid: i64,
    #[serde(default, with = "vec_i64_str")]
    pub dish_ids: Vec<i64>,
    pub dishes: Vec<String>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub grocery_synced: bool,
    #[serde(default)]
    pub done: bool,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuDoc {
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub dishes: Vec<MenuDish>,
}

impl Default for MenuDoc {
    fn default() -> Self {
        Self {
            categories: vec![
                "荤菜".into(),
                "素菜".into(),
                "汤羹".into(),
                "主食".into(),
                "甜饮".into(),
            ],
            dishes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuDish {
    #[serde(with = "i64_str")]
    pub id: i64,
    pub name: String,
    pub category: String,
    #[serde(default, with = "opt_i64_str")]
    pub cover_asset_id: Option<i64>,
    #[serde(default)]
    pub ingredients: Vec<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BoardDoc {
    #[serde(default)]
    pub notes: Vec<BoardNote>,
    #[serde(default)]
    pub todos: Vec<BoardTodo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardNote {
    #[serde(with = "i64_str")]
    pub id: i64,
    #[serde(with = "i64_str")]
    pub uid: i64,
    pub text: String,
    #[serde(default = "default_paper_color")]
    pub color: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub unlock_at: Option<i64>,
    #[serde(default)]
    pub locked: bool,
    pub at: i64,
}

fn default_paper_color() -> String {
    "oat".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardTodo {
    #[serde(with = "i64_str")]
    pub id: i64,
    pub text: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub done: bool,
    #[serde(default, with = "opt_i64_str")]
    pub done_by: Option<i64>,
    #[serde(default)]
    pub done_at: Option<i64>,
    pub at: i64,
}

#[derive(Serialize)]
pub struct VersionedModule<T> {
    pub version: i64,
    pub data: T,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct FridgeBundleResp {
    pub fridge: VersionedModule<FridgeDoc>,
    pub menu: VersionedModule<MenuDoc>,
    pub board: VersionedModule<BoardDoc>,
    pub users: HashMap<String, UserSummaryDto>,
    pub asset_urls: HashMap<String, CachedAssetUrls>,
}

async fn get_fridge_bundle(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<FridgeBundleResp>> {
    build_fridge_bundle_resp(&state, guard.space_id, guard.user_id)
        .await
        .map(Json)
}

#[derive(Deserialize)]
pub struct UpdateMenuReq {
    pub expected_version: i64,
    pub categories: Vec<String>,
    pub dishes: Vec<MenuDishInput>,
}

#[derive(Deserialize)]
pub struct MenuDishInput {
    #[serde(default, with = "opt_i64_str")]
    pub id: Option<i64>,
    pub name: String,
    pub category: String,
    #[serde(default, with = "opt_i64_str")]
    pub cover_asset_id: Option<i64>,
    #[serde(default)]
    pub ingredients: Vec<String>,
    #[serde(default)]
    pub note: String,
}

async fn update_menu(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<UpdateMenuReq>,
) -> AppResult<Json<FridgeBundleResp>> {
    let mut normalized_dishes = Vec::with_capacity(req.dishes.len());
    for d in req.dishes {
        let trimmed = d.name.trim();
        if trimmed.is_empty() {
            continue;
        }
        normalized_dishes.push(MenuDish {
            id: d.id.unwrap_or_else(|| state.snowflake.next_id()),
            name: trimmed.to_string(),
            category: d.category.trim().to_string(),
            cover_asset_id: d.cover_asset_id,
            ingredients: d
                .ingredients
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            note: d.note.trim().to_string(),
        });
    }

    let new_doc = MenuDoc {
        categories: req.categories,
        dishes: normalized_dishes,
    };
    let raw = serde_json::to_string(&new_doc)?;
    let now = now_ms();

    let mut tx = state.db.write.begin().await?;
    let m_row = sqlx::query(
        "SELECT version, data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(guard.space_id)
    .bind(dim::MOD_MENU)
    .fetch_one(&mut *tx)
    .await?;

    let current_ver: i64 = m_row.get("version");
    if current_ver != req.expected_version {
        return Err(AppError::Conflict(
            "菜单刚被其他成员更新过，已自动同步最新状态，请重试".to_string(),
        ));
    }

    let old_menu: MenuDoc = serde_json::from_str(m_row.get("data_json")).unwrap_or_default();
    let mut old_counts: HashMap<i64, i64> = HashMap::new();
    for aid in old_menu.dishes.iter().filter_map(|d| d.cover_asset_id) {
        *old_counts.entry(aid).or_insert(0) += 1;
    }
    let mut new_counts: HashMap<i64, i64> = HashMap::new();
    for aid in new_doc.dishes.iter().filter_map(|d| d.cover_asset_id) {
        *new_counts.entry(aid).or_insert(0) += 1;
    }

    let mut all_aids: HashSet<i64> = old_counts.keys().copied().collect();
    all_aids.extend(new_counts.keys().copied());

    let mut to_release: Vec<(i64, i64)> = Vec::new();
    for aid in all_aids {
        let prev_c = old_counts.get(&aid).copied().unwrap_or(0);
        let next_c = new_counts.get(&aid).copied().unwrap_or(0);
        if next_c > prev_c {
            bind_owned_asset_in_tx(
                &mut tx,
                aid,
                guard.user_id,
                dim::ASSET_DISH_IMG,
                next_c - prev_c,
            )
            .await?;
        } else if prev_c > next_c {
            to_release.push((aid, prev_c - next_c));
        }
    }

    sqlx::query(
        "UPDATE space_modules
         SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4 AND version = ?5",
    )
    .bind(&raw)
    .bind(now)
    .bind(guard.space_id)
    .bind(dim::MOD_MENU)
    .bind(req.expected_version)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    for (rid, dec) in to_release {
        let _ = release_asset_ref_by(&state, rid, dec).await;
    }

    state
        .hub
        .publish(guard.space_id, "FRIDGE_UPDATED", serde_json::json!({}))
        .await;

    build_fridge_bundle_resp(&state, guard.space_id, guard.user_id)
        .await
        .map(Json)
}

#[derive(Deserialize)]
pub struct PinMenuSlipReq {
    #[serde(default, with = "vec_i64_str")]
    pub dish_ids: Vec<i64>,
    #[serde(default)]
    pub custom_dishes: Vec<String>,
    pub date_label: Option<String>,
    #[serde(default)]
    pub note: String,
}

async fn pin_today_menu_slip(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<PinMenuSlipReq>,
) -> AppResult<Json<FridgeBundleResp>> {
    let (_, menu_doc, _) = read_module::<MenuDoc>(&state, guard.space_id, dim::MOD_MENU).await?;
    let dish_lookup: HashMap<i64, String> = menu_doc
        .dishes
        .iter()
        .map(|d| (d.id, d.name.clone()))
        .collect();

    let mut seen_ids = HashSet::new();
    let mut valid_dish_ids = Vec::new();
    let mut dish_names = Vec::new();

    for id in req.dish_ids {
        if seen_ids.insert(id) {
            if let Some(name) = dish_lookup.get(&id) {
                valid_dish_ids.push(id);
                dish_names.push(name.clone());
            }
        }
    }

    for custom in req.custom_dishes {
        let t = custom.trim();
        if !t.is_empty() && !dish_names.iter().any(|x| x == t) {
            dish_names.push(t.to_string());
        }
    }

    if dish_names.is_empty() {
        return Err(AppError::BadRequest("请至少勾选一道今日想吃的菜品".to_string()));
    }

    let now = now_ms();
    let bj_label = req
        .date_label
        .unwrap_or_else(|| format!("{} 今日菜单", now_beijing().format("%m.%d")));

    let new_slip = FridgeSlip {
        id: state.snowflake.next_id(),
        date_label: bj_label.clone(),
        by_uid: guard.user_id,
        dish_ids: valid_dish_ids,
        dishes: dish_names.clone(),
        note: req.note.trim().to_string(),
        grocery_synced: false,
        done: false,
        at: now,
    };

    let mut tx = state.db.write.begin().await?;
    let row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(guard.space_id)
    .bind(dim::MOD_FRIDGE)
    .fetch_one(&mut *tx)
    .await?;

    let mut fridge: FridgeDoc = serde_json::from_str(row.get("data_json")).unwrap_or_default();
    fridge.recent_slips.insert(0, new_slip.clone());
    fridge.recent_slips.truncate(MAX_RECENT_FRIDGE_SLIPS);

    let new_raw = serde_json::to_string(&fridge)?;
    sqlx::query(
        "UPDATE space_modules
         SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4",
    )
    .bind(&new_raw)
    .bind(now)
    .bind(guard.space_id)
    .bind(dim::MOD_FRIDGE)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    state
        .hub
        .publish(
            guard.space_id,
            "FRIDGE_UPDATED",
            serde_json::to_value(&fridge).unwrap_or_default(),
        )
        .await;

    let _ = record_timeline_event(
        &state,
        guard.space_id,
        guard.user_id,
        dim::EVT_MENU_PINNED,
        serde_json::json!({
            "slip_id": new_slip.id.to_string(),
            "date_label": bj_label,
            "dishes": dish_names,
            "note": new_slip.note,
        }),
    )
    .await;

    if guard.is_shared() {
        let _ = state.pet_reward_tx.send(PetLifeEvent {
            space_id: guard.space_id,
            actor_id: guard.user_id,
            action: PetLifeTrigger::MenuPinned,
        });
    }

    build_fridge_bundle_resp(&state, guard.space_id, guard.user_id)
        .await
        .map(Json)
}

#[derive(Deserialize)]
pub struct ToggleSlipDoneReq {
    pub done: bool,
}

async fn toggle_slip_done(
    guard: SpaceGuard,
    Path((_sid, raw_slip_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<ToggleSlipDoneReq>,
) -> AppResult<Json<FridgeBundleResp>> {
    let slip_id = raw_slip_id
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的菜单便签 ID".to_string()))?;

    let mut tx = state.db.write.begin().await?;
    let row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(guard.space_id)
    .bind(dim::MOD_FRIDGE)
    .fetch_one(&mut *tx)
    .await?;

    let mut fridge: FridgeDoc = serde_json::from_str(row.get("data_json")).unwrap_or_default();
    let slip = fridge
        .recent_slips
        .iter_mut()
        .find(|s| s.id == slip_id)
        .ok_or_else(|| AppError::NotFound("未在冰箱门上找到该菜单便签".to_string()))?;
    slip.done = req.done;

    let raw = serde_json::to_string(&fridge)?;
    sqlx::query(
        "UPDATE space_modules
         SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4",
    )
    .bind(&raw)
    .bind(now_ms())
    .bind(guard.space_id)
    .bind(dim::MOD_FRIDGE)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    state
        .hub
        .publish(guard.space_id, "FRIDGE_UPDATED", serde_json::json!({}))
        .await;

    build_fridge_bundle_resp(&state, guard.space_id, guard.user_id)
        .await
        .map(Json)
}

async fn convert_slip_to_grocery(
    guard: SpaceGuard,
    Path((_sid, raw_slip_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> AppResult<Json<FridgeBundleResp>> {
    let slip_id = raw_slip_id
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的菜单便签 ID".to_string()))?;

    let now = now_ms();
    let mut tx = state.db.write.begin().await?;

    let m_row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(guard.space_id)
    .bind(dim::MOD_MENU)
    .fetch_one(&mut *tx)
    .await?;
    let menu_doc: MenuDoc = serde_json::from_str(m_row.get("data_json")).unwrap_or_default();

    let f_row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(guard.space_id)
    .bind(dim::MOD_FRIDGE)
    .fetch_one(&mut *tx)
    .await?;
    let mut fridge: FridgeDoc = serde_json::from_str(f_row.get("data_json")).unwrap_or_default();

    let slip = fridge
        .recent_slips
        .iter_mut()
        .find(|s| s.id == slip_id)
        .ok_or_else(|| AppError::NotFound("未找到该菜单便签".to_string()))?;

    let mut needed_ingredients = Vec::new();
    let mut covered_dish_names = HashSet::new();

    for did in &slip.dish_ids {
        if let Some(d) = menu_doc.dishes.iter().find(|m| m.id == *did) {
            covered_dish_names.insert(d.name.clone());
            if d.ingredients.is_empty() {
                needed_ingredients.push(format!("备菜：{}", d.name));
            } else {
                for ing in &d.ingredients {
                    needed_ingredients.push(ing.clone());
                }
            }
        }
    }
    // Also preserve custom dishes or dishes later removed from MenuDoc!
    for name in &slip.dishes {
        if !covered_dish_names.contains(name) {
            needed_ingredients.push(format!("买菜：{}", name));
        }
    }
    slip.grocery_synced = true;

    let b_row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(guard.space_id)
    .bind(dim::MOD_BOARD)
    .fetch_one(&mut *tx)
    .await?;
    let mut board: BoardDoc = serde_json::from_str(b_row.get("data_json")).unwrap_or_default();

    let existing_open: HashSet<String> = board
        .todos
        .iter()
        .filter(|t| !t.done)
        .map(|t| t.text.clone())
        .collect();

    let mut uniq_added = HashSet::new();
    for ing in needed_ingredients {
        if !existing_open.contains(&ing) && uniq_added.insert(ing.clone()) {
            board.todos.insert(
                0,
                BoardTodo {
                    id: state.snowflake.next_id(),
                    text: ing,
                    category: Some("买菜采购".to_string()),
                    done: false,
                    done_by: None,
                    done_at: None,
                    at: now,
                },
            );
        }
    }

    sqlx::query(
        "UPDATE space_modules SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4",
    )
    .bind(serde_json::to_string(&fridge)?)
    .bind(now)
    .bind(guard.space_id)
    .bind(dim::MOD_FRIDGE)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE space_modules SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4",
    )
    .bind(serde_json::to_string(&board)?)
    .bind(now)
    .bind(guard.space_id)
    .bind(dim::MOD_BOARD)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    state
        .hub
        .publish(guard.space_id, "FRIDGE_UPDATED", serde_json::json!({}))
        .await;

    build_fridge_bundle_resp(&state, guard.space_id, guard.user_id)
        .await
        .map(Json)
}

#[derive(Deserialize)]
pub struct UpdateBoardReq {
    pub expected_version: i64,
    pub notes: Vec<BoardNoteInput>,
    pub todos: Vec<BoardTodoInput>,
}

#[derive(Deserialize)]
pub struct BoardNoteInput {
    #[serde(default, with = "opt_i64_str")]
    pub id: Option<i64>,
    #[serde(default, with = "opt_i64_str")]
    pub uid: Option<i64>,
    pub text: String,
    #[serde(default = "default_paper_color")]
    pub color: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub unlock_at: Option<i64>,
    pub at: Option<i64>,
}

#[derive(Deserialize)]
pub struct BoardTodoInput {
    #[serde(default, with = "opt_i64_str")]
    pub id: Option<i64>,
    pub text: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub done: bool,
    #[serde(default, with = "opt_i64_str")]
    pub done_by: Option<i64>,
    pub at: Option<i64>,
}

async fn update_board(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<UpdateBoardReq>,
) -> AppResult<Json<FridgeBundleResp>> {
    let now = now_ms();
    let mut tx = state.db.write.begin().await?;

    let b_row = sqlx::query(
        "SELECT version, data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(guard.space_id)
    .bind(dim::MOD_BOARD)
    .fetch_one(&mut *tx)
    .await?;

    let current_ver: i64 = b_row.get("version");
    if current_ver != req.expected_version {
        return Err(AppError::Conflict(
            "留言/待办板刚被其他成员更新过，已自动同步最新状态，请重试".to_string(),
        ));
    }

    let old_board: BoardDoc = serde_json::from_str(b_row.get("data_json")).unwrap_or_default();
    let old_done_ids: HashSet<i64> = old_board
        .todos
        .iter()
        .filter(|t| t.done)
        .map(|t| t.id)
        .collect();
    let old_todos_map: HashMap<i64, BoardTodo> =
        old_board.todos.into_iter().map(|t| (t.id, t)).collect();
    let old_notes_map: HashMap<i64, BoardNote> =
        old_board.notes.into_iter().map(|n| (n.id, n)).collect();

    let mut notes = Vec::with_capacity(req.notes.len());
    let mut seen_existing = HashSet::new();

    for n in req.notes {
        if let Some(existing_id) = n.id {
            if let Some(prev) = old_notes_map.get(&existing_id) {
                seen_existing.insert(existing_id);
                if prev.uid != guard.user_id {
                    // Non-authors can NEVER overwrite another member's note text/color/unlock_at/uid
                    // (prevents redacted time-capsule text `""` from ever wiping the real message!)
                    let mut preserved = prev.clone();
                    preserved.pinned = n.pinned;
                    preserved.locked = false;
                    notes.push(preserved);
                } else {
                    notes.push(BoardNote {
                        id: prev.id,
                        uid: prev.uid,
                        text: n.text,
                        color: n.color,
                        pinned: n.pinned,
                        unlock_at: n.unlock_at,
                        locked: false,
                        at: prev.at,
                    });
                }
                continue;
            }
        }

        // Brand-new note: strictly bind `uid` to `guard.user_id` (prevents uid spoofing!)
        let _ = n.uid;
        let _ = n.at;
        if n.text.trim().is_empty() {
            continue;
        }
        notes.push(BoardNote {
            id: state.snowflake.next_id(),
            uid: guard.user_id,
            text: n.text,
            color: n.color,
            pinned: n.pinned,
            unlock_at: n.unlock_at,
            locked: false,
            at: now,
        });
    }

    // Protect locked time-capsule notes of OTHER members from accidental deletion before unlock!
    for (old_id, prev) in &old_notes_map {
        let still_locked = prev.unlock_at.map(|u| now < u).unwrap_or(false);
        if still_locked && prev.uid != guard.user_id && !seen_existing.contains(old_id) {
            notes.push(prev.clone());
        }
    }

    let mut newly_completed_count = 0usize;
    let mut todos = Vec::with_capacity(req.todos.len());
    for t in req.todos {
        let trimmed = t.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let existing_todo = t.id.and_then(|id| old_todos_map.get(&id));
        let tid = existing_todo
            .map(|p| p.id)
            .unwrap_or_else(|| state.snowflake.next_id());
        let (done_by, done_at) = if t.done {
            if !old_done_ids.contains(&tid) {
                newly_completed_count += 1;
                (Some(guard.user_id), Some(now))
            } else {
                (
                    existing_todo.and_then(|p| p.done_by).or(Some(guard.user_id)),
                    existing_todo.and_then(|p| p.done_at).or(Some(now)),
                )
            }
        } else {
            (None, None)
        };
        let _ = t.done_by;
        todos.push(BoardTodo {
            id: tid,
            text: trimmed.to_string(),
            category: t.category,
            done: t.done,
            done_by,
            done_at,
            at: existing_todo.map(|p| p.at).unwrap_or(now),
        });
    }

    // Keep active todos in user order at top, and archived todos ordered by completion time desc
    let (mut open_todos, mut archived_todos): (Vec<BoardTodo>, Vec<BoardTodo>) =
        todos.into_iter().partition(|t| !t.done);
    archived_todos.sort_by(|a, b| b.done_at.unwrap_or(b.at).cmp(&a.done_at.unwrap_or(a.at)));
    archived_todos.truncate(200);
    open_todos.extend(archived_todos);
    let todos = open_todos;

    let saved_board = BoardDoc { notes, todos };
    let raw = serde_json::to_string(&saved_board)?;

    sqlx::query(
        "UPDATE space_modules
         SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4 AND version = ?5",
    )
    .bind(&raw)
    .bind(now)
    .bind(guard.space_id)
    .bind(dim::MOD_BOARD)
    .bind(req.expected_version)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    state
        .hub
        .publish(guard.space_id, "FRIDGE_UPDATED", serde_json::json!({}))
        .await;

    let _ = record_timeline_event(
        &state,
        guard.space_id,
        guard.user_id,
        dim::EVT_BOARD_UPDATED,
        serde_json::json!({ "newly_completed_todos": newly_completed_count }),
    )
    .await;

    if guard.is_shared() && newly_completed_count > 0 {
        let _ = state.pet_reward_tx.send(PetLifeEvent {
            space_id: guard.space_id,
            actor_id: guard.user_id,
            action: PetLifeTrigger::TodoCompleted,
        });
    }

    build_fridge_bundle_resp(&state, guard.space_id, guard.user_id)
        .await
        .map(Json)
}

#[derive(Deserialize)]
pub struct ProjectDishReq {
    #[serde(with = "i64_str")]
    pub dish_id: i64,
}

async fn project_dish_to_space(
    guard: SpaceGuard,
    Path((_sid, raw_target_sid)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(req): Json<ProjectDishReq>,
) -> AppResult<Json<serde_json::Value>> {
    let target_space_id = raw_target_sid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("无效的目标空间 ID".to_string()))?;

    if target_space_id == guard.space_id {
        return Err(AppError::BadRequest("目标空间不能与当前空间相同".to_string()));
    }

    let has_acc = sqlx::query(
        "SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2",
    )
    .bind(target_space_id)
    .bind(guard.user_id)
    .fetch_optional(&state.db.read)
    .await?;
    if has_acc.is_none() {
        return Err(AppError::NotFound("目标空间不存在或无访问权限".to_string()));
    }

    let mut tx = state.db.write.begin().await?;
    let s_row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(guard.space_id)
    .bind(dim::MOD_MENU)
    .fetch_one(&mut *tx)
    .await?;
    let src_menu: MenuDoc = serde_json::from_str(s_row.get("data_json")).unwrap_or_default();

    let src_dish = src_menu
        .dishes
        .into_iter()
        .find(|d| d.id == req.dish_id)
        .ok_or_else(|| AppError::NotFound("原菜单中未找到该菜品".to_string()))?;

    if let Some(cid) = src_dish.cover_asset_id {
        clone_existing_asset_ref_in_tx(&mut tx, cid).await?;
    }

    let t_row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(target_space_id)
    .bind(dim::MOD_MENU)
    .fetch_one(&mut *tx)
    .await?;
    let mut target_menu: MenuDoc = serde_json::from_str(t_row.get("data_json")).unwrap_or_default();

    let new_dish_id = state.snowflake.next_id();
    target_menu.dishes.push(MenuDish {
        id: new_dish_id,
        ..src_dish
    });

    sqlx::query(
        "UPDATE space_modules SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4",
    )
    .bind(serde_json::to_string(&target_menu)?)
    .bind(now_ms())
    .bind(target_space_id)
    .bind(dim::MOD_MENU)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    state
        .hub
        .publish(target_space_id, "FRIDGE_UPDATED", serde_json::json!({}))
        .await;

    Ok(Json(serde_json::json!({
        "projected": true,
        "new_dish_id": new_dish_id.to_string(),
        "target_space_id": target_space_id.to_string()
    })))
}

pub async fn read_module<T: serde::de::DeserializeOwned + Default>(
    state: &AppState,
    space_id: i64,
    module_type_id: i64,
) -> AppResult<(i64, T, i64)> {
    let row = sqlx::query(
        "SELECT version, data_json, updated_at
         FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(space_id)
    .bind(module_type_id)
    .fetch_optional(&state.db.read)
    .await?
    .ok_or_else(|| AppError::NotFound("模块未初始化".to_string()))?;

    let ver: i64 = row.get("version");
    let raw: String = row.get("data_json");
    let updated_at: i64 = row.get("updated_at");
    let doc = serde_json::from_str::<T>(&raw).unwrap_or_default();
    Ok((ver, doc, updated_at))
}

async fn build_fridge_bundle_resp(
    state: &AppState,
    space_id: i64,
    viewer_uid: i64,
) -> AppResult<FridgeBundleResp> {
    let (f_ver, fridge, f_ts) = read_module::<FridgeDoc>(state, space_id, dim::MOD_FRIDGE).await?;
    let (m_ver, menu, m_ts) = read_module::<MenuDoc>(state, space_id, dim::MOD_MENU).await?;
    let (b_ver, mut board, b_ts) = read_module::<BoardDoc>(state, space_id, dim::MOD_BOARD).await?;

    let now = now_ms();
    let mut uids = Vec::new();

    for note in &mut board.notes {
        uids.push(note.uid);
        if let Some(unlock_ts) = note.unlock_at {
            if now < unlock_ts {
                note.locked = true;
                if note.uid != viewer_uid {
                    note.text = String::new();
                }
            } else {
                note.locked = false;
            }
        }
    }

    for slip in &fridge.recent_slips {
        uids.push(slip.by_uid);
    }
    for todo in &board.todos {
        if let Some(u) = todo.done_by {
            uids.push(u);
        }
    }

    let mut asset_ids: Vec<i64> = menu
        .dishes
        .iter()
        .filter_map(|d| d.cover_asset_id)
        .collect();

    let (users, mut avatar_assets) = load_users_map(state, &uids).await?;
    asset_ids.append(&mut avatar_assets);
    let asset_urls = resolve_asset_urls(state, &asset_ids).await?;

    Ok(FridgeBundleResp {
        fridge: VersionedModule {
            version: f_ver,
            data: fridge,
            updated_at: f_ts,
        },
        menu: VersionedModule {
            version: m_ver,
            data: menu,
            updated_at: m_ts,
        },
        board: VersionedModule {
            version: b_ver,
            data: board,
            updated_at: b_ts,
        },
        users,
        asset_urls,
    })
}
