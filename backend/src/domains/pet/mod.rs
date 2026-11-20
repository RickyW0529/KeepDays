pub mod engine;
pub mod model;

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use rand::seq::SliceRandom;
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::{HashMap, HashSet};

use crate::{
    domains::{
        fridge::{read_module, BoardDoc, MenuDish, MenuDoc},
        media::resolve_asset_urls,
        pet::{
            engine::evaluate_pet_lazy_tick,
            model::{
                find_shop_item, CoopQuest, PetLogEntry, PetStateDoc, ShopItemDef, SHOP_CATALOG,
            },
        },
        profile::{load_users_map, UserSummaryDto},
        timeline::record_timeline_event,
    },
    error::{AppError, AppResult},
    infra::{cache::CachedAssetUrls, dict::dim},
    middleware::guards::SpaceGuard,
    state::AppState,
    time_bj::now_ms,
};

const VALID_SPECIES: &[&str] = &["BLACK_CAT", "SHIBA", "OTTER", "BUNNY"];

pub fn space_router() -> Router<AppState> {
    Router::new()
        .route("/", get(get_pet_state))
        .route("/adopt", post(adopt_pet))
        .route("/interact", post(interact_pet))
        .route("/shop/buy", post(buy_shop_item))
        .route("/equip", post(equip_decor))
        .route("/adventure", post(start_adventure))
        .route("/paw-pick-menu", post(paw_pick_today_menu))
        .nest("/agent", crate::agent::space_agent_router())
}

#[derive(Serialize)]
pub struct PetSkillsDto {
    pub can_paw_pick_menu: bool,
    pub has_unread_board_letter: bool,
}

#[derive(Serialize)]
pub struct PetFullResp {
    pub pet: PetStateDoc,
    pub skills: PetSkillsDto,
    pub shop_catalog: &'static [ShopItemDef],
    pub sweet_resonance_triggered: bool,
    pub users: HashMap<String, UserSummaryDto>,
    pub asset_urls: HashMap<String, CachedAssetUrls>,
}

fn ensure_shared_space(guard: &SpaceGuard) -> AppResult<()> {
    if !guard.is_shared() {
        return Err(AppError::BadRequest(
            "宠物是共享空间的专属守护生命，仅在建立共享空间后可共同领养".to_string(),
        ));
    }
    Ok(())
}

async fn load_pet_in_tx(tx: &mut Transaction<'_, Sqlite>, space_id: i64) -> AppResult<PetStateDoc> {
    let row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(space_id)
    .bind(dim::MOD_PET)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::NotFound("宠物模块未初始化".to_string()))?;

    Ok(serde_json::from_str(row.get("data_json")).unwrap_or_default())
}

async fn save_pet_in_tx(
    tx: &mut Transaction<'_, Sqlite>,
    space_id: i64,
    pet: &PetStateDoc,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE space_modules SET data_json = ?1, version = version + 1, updated_at = ?2
         WHERE space_id = ?3 AND module_type_id = ?4",
    )
    .bind(serde_json::to_string(pet)?)
    .bind(now_ms())
    .bind(space_id)
    .bind(dim::MOD_PET)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn get_pet_state(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<PetFullResp>> {
    ensure_shared_space(&guard)?;

    let mut tx = state.db.write.begin().await?;
    let mut pet = load_pet_in_tx(&mut tx, guard.space_id).await?;
    if evaluate_pet_lazy_tick(&state, guard.space_id, &mut pet).await {
        save_pet_in_tx(&mut tx, guard.space_id, &pet).await?;
    }
    tx.commit().await?;

    build_pet_resp(&state, guard.space_id, guard.user_id, pet, false)
        .await
        .map(Json)
}

#[derive(Deserialize)]
pub struct AdoptPetReq {
    pub name: String,
    pub species: Option<String>,
}

async fn adopt_pet(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<AdoptPetReq>,
) -> AppResult<Json<PetFullResp>> {
    ensure_shared_space(&guard)?;

    let clean_name = req.name.trim();
    if clean_name.is_empty() || clean_name.chars().count() > 16 {
        return Err(AppError::BadRequest("请给宠物起一个 1~16 字的名字".to_string()));
    }

    let species = req
        .species
        .as_deref()
        .unwrap_or("BLACK_CAT")
        .trim()
        .to_uppercase();
    if !VALID_SPECIES.contains(&species.as_str()) {
        return Err(AppError::BadRequest("不支持的宠物品种".to_string()));
    }

    let now = now_ms();
    let mut tx = state.db.write.begin().await?;
    let mut pet = load_pet_in_tx(&mut tx, guard.space_id).await?;
    if pet.adopted {
        return Err(AppError::Conflict("这个共享空间已经领养过专属宠物啦".to_string()));
    }

    pet.adopted = true;
    pet.name = clean_name.to_string();
    pet.species = species;
    pet.adopted_at = Some(now);
    pet.last_tick_at = now;
    pet.current.behavior = "ROAMING_WALL".to_string();
    pet.push_log(PetLogEntry {
        uid: guard.user_id,
        action: "ADOPT".to_string(),
        text: format!("欢迎「{}」正式入住共享小窝", pet.name),
        resonance: false,
        at: now,
    });

    save_pet_in_tx(&mut tx, guard.space_id, &pet).await?;
    tx.commit().await?;

    let _ = record_timeline_event(
        &state,
        guard.space_id,
        guard.user_id,
        dim::EVT_PET_MOMENT,
        serde_json::json!({
            "action": "ADOPTED",
            "pet_name": pet.name,
            "species": pet.species,
        }),
    )
    .await;

    state
        .hub
        .publish(
            guard.space_id,
            "PET_UPDATED",
            serde_json::to_value(&pet).unwrap_or_default(),
        )
        .await;

    build_pet_resp(&state, guard.space_id, guard.user_id, pet, false)
        .await
        .map(Json)
}

#[derive(Deserialize)]
pub struct InteractPetReq {
    pub action: String,
    pub item_id: Option<String>,
}

async fn interact_pet(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<InteractPetReq>,
) -> AppResult<Json<PetFullResp>> {
    ensure_shared_space(&guard)?;

    let now = now_ms();
    let mut tx = state.db.write.begin().await?;
    let mut pet = load_pet_in_tx(&mut tx, guard.space_id).await?;
    if !pet.adopted {
        return Err(AppError::BadRequest("请先完成宠物领养".to_string()));
    }
    let _ = evaluate_pet_lazy_tick(&state, guard.space_id, &mut pet).await;

    if pet.current.behavior == "ADVENTURING" {
        return Err(AppError::BadRequest(
            "小家伙正在外面散步探险，等它回来再互动吧".to_string(),
        ));
    }

    let act = req.action.trim().to_uppercase();

    // Validate action pre-conditions BEFORE recording resonance touch!
    match act.as_str() {
        "STROKE" => {}
        "PLAY" => {
            if pet.stats.energy < 20 {
                return Err(AppError::BadRequest(
                    "它有点犯困啦，喂点温羊奶或让它休息一会儿吧".to_string(),
                ));
            }
        }
        "FEED" => {
            let item_id = req.item_id.as_deref().unwrap_or("salmon_freeze_dried");
            let def = find_shop_item(item_id)
                .filter(|d| d.category == "FOOD")
                .ok_or_else(|| AppError::BadRequest("未知的口粮道具".to_string()))?;
            if pet.inventory.get(item_id).copied().unwrap_or(0) == 0 {
                return Err(AppError::BadRequest(format!(
                    "背包里的「{}」吃光啦，去杂货铺兑一点吧",
                    def.name
                )));
            }
        }
        "OPEN_DUO_CAN" => {
            if let Some(q) = &pet.coop_quest {
                if now <= q.expires_at {
                    return Err(AppError::Conflict(
                        "已经有一罐双拼罐头打开着，等伙伴接力完成吧".to_string(),
                    ));
                }
            }
            if pet.inventory.get("duo_feast_can").copied().unwrap_or(0) == 0 {
                return Err(AppError::BadRequest("背包里没有双拼鲜肉罐头啦".to_string()));
            }
        }
        "FINISH_DUO_CAN" => {
            let q = pet
                .coop_quest
                .as_ref()
                .filter(|q| now <= q.expires_at)
                .ok_or_else(|| AppError::BadRequest("当前没有待合喂的双拼罐头".to_string()))?;
            if q.step1_by == guard.user_id {
                return Err(AppError::BadRequest(
                    "双人合喂需要空间里的另一位成员来接力完成".to_string(),
                ));
            }
        }
        _ => return Err(AppError::BadRequest("不支持的宠物互动指令".to_string())),
    }

    let partner_uid = state
        .cache
        .record_pet_touch(guard.space_id, guard.user_id, now)
        .await;
    let resonance = partner_uid.is_some();
    let mult: u32 = if resonance { 2 } else { 1 };
    let log_msg: String;

    match act.as_str() {
        "STROKE" => {
            pet.stats.mood = (pet.stats.mood.saturating_add(8)).min(100);
            pet.stats.bond += 6 * mult;
            pet.add_exp(8 * mult);
            pet.traits.clingy_vs_independent =
                (pet.traits.clingy_vs_independent + 2).clamp(-100, 100);
            log_msg = if resonance {
                format!("顺了顺{}的毛，触发双人心有灵犀共振", pet.name)
            } else {
                format!("摸了摸{}的下巴，舒服地打起了呼噜", pet.name)
            };
        }
        "PLAY" => {
            pet.stats.energy -= 10;
            pet.stats.mood = (pet.stats.mood.saturating_add(14)).min(100);
            pet.stats.bond += 8 * mult;
            pet.add_exp(14 * mult);
            pet.traits.foodie_vs_athletic = (pet.traits.foodie_vs_athletic - 3).clamp(-100, 100);
            log_msg = format!("陪{}玩了一会儿", pet.name);
        }
        "FEED" => {
            let item_id = req.item_id.as_deref().unwrap_or("salmon_freeze_dried");
            let def = find_shop_item(item_id).unwrap();
            let count = pet.inventory.get(item_id).copied().unwrap_or(1);
            pet.inventory.insert(item_id.to_string(), count - 1);
            pet.stats.satiety = (pet.stats.satiety.saturating_add(def.satiety_gain)).min(100);
            pet.stats.mood = (pet.stats.mood.saturating_add(def.mood_gain)).min(100);
            pet.stats.energy = (pet.stats.energy.saturating_add(def.energy_gain)).min(100);
            pet.stats.bond += 8 * mult;
            pet.add_exp(12 * mult);
            pet.traits.foodie_vs_athletic = (pet.traits.foodie_vs_athletic + 3).clamp(-100, 100);
            log_msg = format!("喂了一份「{}」", def.name);
        }
        "OPEN_DUO_CAN" => {
            let count = pet.inventory.get("duo_feast_can").copied().unwrap_or(1);
            pet.inventory.insert("duo_feast_can".to_string(), count - 1);
            pet.coop_quest = Some(CoopQuest {
                quest_id: "q_duo_can".to_string(),
                title: "双人合喂鲜肉双拼罐".to_string(),
                step1_by: guard.user_id,
                step1_at: now,
                expires_at: now + 12 * 3600 * 1000,
            });
            log_msg = format!("拉开了双拼罐头，等待另一半接力投喂{}", pet.name);
        }
        "FINISH_DUO_CAN" => {
            pet.coop_quest = None;
            pet.stats.satiety = 100;
            pet.stats.mood = 100;
            pet.stats.energy = (pet.stats.energy.saturating_add(30)).min(100);
            pet.stats.bond += 40;
            pet.paw_coins += 15;
            pet.add_exp(50);
            log_msg = format!("默契接力完成了双拼罐合喂，{}满足地眯起眼睛", pet.name);
        }
        _ => unreachable!(),
    }

    if resonance {
        pet.paw_coins += 10;
    }

    pet.push_log(PetLogEntry {
        uid: guard.user_id,
        action: act,
        text: log_msg,
        resonance,
        at: now,
    });

    save_pet_in_tx(&mut tx, guard.space_id, &pet).await?;
    tx.commit().await?;

    state
        .hub
        .publish(
            guard.space_id,
            "PET_UPDATED",
            serde_json::json!({
                "pet": &pet,
                "sweet_resonance": resonance,
                "actor_id": guard.user_id.to_string(),
            }),
        )
        .await;

    build_pet_resp(&state, guard.space_id, guard.user_id, pet, resonance)
        .await
        .map(Json)
}

#[derive(Deserialize)]
pub struct BuyShopReq {
    pub item_id: String,
    pub quantity: Option<u32>,
}

async fn buy_shop_item(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<BuyShopReq>,
) -> AppResult<Json<PetFullResp>> {
    ensure_shared_space(&guard)?;

    let qty = req.quantity.unwrap_or(1).clamp(1, 20);
    let def = find_shop_item(&req.item_id)
        .ok_or_else(|| AppError::BadRequest("杂货铺中没有该物品".to_string()))?;

    let mut tx = state.db.write.begin().await?;
    let mut pet = load_pet_in_tx(&mut tx, guard.space_id).await?;
    if !pet.adopted {
        return Err(AppError::BadRequest("请先领养宠物".to_string()));
    }

    let effective_qty = if matches!(def.category, "BED" | "ACCESSORY") {
        1
    } else {
        qty
    };
    let total_cost = def.price_coins * (effective_qty as i64);
    if pet.paw_coins < total_cost {
        return Err(AppError::BadRequest(format!(
            "爪印币不足（当前 {}，需要 {}）",
            pet.paw_coins, total_cost
        )));
    }

    if matches!(def.category, "BED" | "ACCESSORY") {
        if pet.unlocked_decor.contains(&req.item_id) {
            return Err(AppError::Conflict("这件装扮/窝垫已经拥有啦".to_string()));
        }
        pet.paw_coins -= def.price_coins;
        pet.unlocked_decor.push(req.item_id.clone());
        if def.category == "BED" {
            pet.current.bed_style = req.item_id.clone();
        } else {
            pet.current.equipped_accessory = Some(req.item_id.clone());
        }
    } else {
        pet.paw_coins -= total_cost;
        *pet.inventory.entry(req.item_id.clone()).or_insert(0) += effective_qty;
    }

    pet.stats.mood = (pet.stats.mood.saturating_add(def.mood_gain)).min(100);
    save_pet_in_tx(&mut tx, guard.space_id, &pet).await?;
    tx.commit().await?;

    state
        .hub
        .publish(
            guard.space_id,
            "PET_UPDATED",
            serde_json::to_value(&pet).unwrap_or_default(),
        )
        .await;

    build_pet_resp(&state, guard.space_id, guard.user_id, pet, false)
        .await
        .map(Json)
}

fn deserialize_double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
pub struct EquipDecorReq {
    pub bed_style: Option<String>,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub accessory: Option<Option<String>>,
}

async fn equip_decor(
    guard: SpaceGuard,
    State(state): State<AppState>,
    Json(req): Json<EquipDecorReq>,
) -> AppResult<Json<PetFullResp>> {
    ensure_shared_space(&guard)?;

    let mut tx = state.db.write.begin().await?;
    let mut pet = load_pet_in_tx(&mut tx, guard.space_id).await?;
    if !pet.adopted {
        return Err(AppError::BadRequest("请先领养宠物".to_string()));
    }

    if let Some(bed) = req.bed_style {
        if !pet.unlocked_decor.contains(&bed) {
            return Err(AppError::BadRequest("尚未解锁该窝垫样式".to_string()));
        }
        pet.current.bed_style = bed;
    }

    if let Some(acc_opt) = req.accessory {
        match acc_opt.filter(|a| !a.is_empty()) {
            Some(acc) => {
                let is_valid_acc = find_shop_item(&acc)
                    .map(|d| d.category == "ACCESSORY")
                    .unwrap_or(false);
                if !is_valid_acc || !pet.unlocked_decor.contains(&acc) {
                    return Err(AppError::BadRequest("尚未解锁该配饰".to_string()));
                }
                pet.current.equipped_accessory = Some(acc);
            }
            None => {
                pet.current.equipped_accessory = None;
            }
        }
    }

    save_pet_in_tx(&mut tx, guard.space_id, &pet).await?;
    tx.commit().await?;

    state
        .hub
        .publish(
            guard.space_id,
            "PET_UPDATED",
            serde_json::to_value(&pet).unwrap_or_default(),
        )
        .await;

    build_pet_resp(&state, guard.space_id, guard.user_id, pet, false)
        .await
        .map(Json)
}

async fn start_adventure(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<PetFullResp>> {
    ensure_shared_space(&guard)?;

    let now = now_ms();
    let mut tx = state.db.write.begin().await?;
    let mut pet = load_pet_in_tx(&mut tx, guard.space_id).await?;
    if !pet.adopted {
        return Err(AppError::BadRequest("请先领养宠物".to_string()));
    }
    if pet.current.behavior == "ADVENTURING" {
        return Err(AppError::Conflict("它已经在外面探险啦".to_string()));
    }
    if pet.stats.energy < 40 {
        return Err(AppError::BadRequest(
            "精力不足 40，先喂点温热羊奶再出发吧".to_string(),
        ));
    }

    let base_duration_ms: i64 = 90 * 60 * 1000;
    let speed_factor = if pet.traits.foodie_vs_athletic < -20 {
        0.7
    } else {
        1.0
    };
    let duration_ms = ((base_duration_ms as f64) * speed_factor) as i64;

    pet.stats.energy -= 30;
    pet.current.behavior = "ADVENTURING".to_string();
    pet.current.focus_photo_id = None;
    pet.current.carrying_old_photo_id = None;
    pet.current.carrying_old_asset_id = None;
    pet.current.adventure_started_at = Some(now);
    pet.current.adventure_return_at = Some(now + duration_ms);
    pet.push_log(PetLogEntry {
        uid: guard.user_id,
        action: "ADVENTURE".to_string(),
        text: format!("背上小包出门散步探险啦"),
        resonance: false,
        at: now,
    });

    save_pet_in_tx(&mut tx, guard.space_id, &pet).await?;
    tx.commit().await?;

    state
        .hub
        .publish(
            guard.space_id,
            "PET_UPDATED",
            serde_json::to_value(&pet).unwrap_or_default(),
        )
        .await;

    build_pet_resp(&state, guard.space_id, guard.user_id, pet, false)
        .await
        .map(Json)
}

#[derive(Serialize)]
pub struct PawPickMenuResp {
    pub pet_name: String,
    pub pet_commentary: String,
    #[serde(with = "crate::snowflake::vec_i64_str")]
    pub picked_dish_ids: Vec<i64>,
    pub picked_dishes: Vec<MenuDish>,
}

async fn paw_pick_today_menu(
    guard: SpaceGuard,
    State(state): State<AppState>,
) -> AppResult<Json<PawPickMenuResp>> {
    ensure_shared_space(&guard)?;

    let (_, pet, _) = read_module::<PetStateDoc>(&state, guard.space_id, dim::MOD_PET).await?;
    if !pet.adopted {
        return Err(AppError::BadRequest("先领养宠物才能请它帮选菜单".to_string()));
    }
    if pet.current.behavior == "ADVENTURING" {
        return Err(AppError::BadRequest(
            "小家伙正在外面散步探险，暂时没法伸爪选菜哦".to_string(),
        ));
    }

    let (_, menu, _) = read_module::<MenuDoc>(&state, guard.space_id, dim::MOD_MENU).await?;
    if menu.dishes.is_empty() {
        return Err(AppError::BadRequest(
            "家庭菜单还是空的，先添加几道菜吧".to_string(),
        ));
    }

    let mut picked = Vec::new();
    let mut picked_set = HashSet::new();
    let mut rng = rand::thread_rng();

    for target_cat in ["荤菜", "素菜", "汤羹"] {
        let candidates: Vec<&MenuDish> = menu
            .dishes
            .iter()
            .filter(|d| d.category == target_cat)
            .collect();
        if let Some(c) = candidates.choose(&mut rng) {
            if picked_set.insert(c.id) {
                picked.push((*c).clone());
            }
        }
    }

    // Top up from remaining dishes (e.g. 主食 / 甜饮 / custom categories) if fewer than 2 matched
    if picked.len() < 2 && menu.dishes.len() > picked.len() {
        let remaining: Vec<&MenuDish> = menu
            .dishes
            .iter()
            .filter(|d| !picked_set.contains(&d.id))
            .collect();
        let need = (2 - picked.len()).min(remaining.len());
        for d in remaining.choose_multiple(&mut rng, need) {
            picked_set.insert(d.id);
            picked.push((*d).clone());
        }
    }

    let picked_ids: Vec<i64> = picked.iter().map(|d| d.id).collect();
    let names: Vec<String> = picked.iter().map(|d| d.name.clone()).collect();

    Ok(Json(PawPickMenuResp {
        pet_name: pet.name.clone(),
        pet_commentary: format!(
            "{}伸爪按下了 {} 道菜：{}",
            pet.name,
            names.len(),
            names.join(" · ")
        ),
        picked_dish_ids: picked_ids,
        picked_dishes: picked,
    }))
}

async fn build_pet_resp(
    state: &AppState,
    space_id: i64,
    viewer_uid: i64,
    pet: PetStateDoc,
    sweet_resonance_triggered: bool,
) -> AppResult<PetFullResp> {
    let (_, board, _) = read_module::<BoardDoc>(state, space_id, dim::MOD_BOARD)
        .await
        .unwrap_or((1, BoardDoc::default(), 0));

    let now = now_ms();
    // Respect `unlock_at` so time-capsule notes never leak early, and newly-unlocked notes DO light up the envelope!
    let has_unread_board_letter = board.notes.iter().any(|n| {
        if n.uid == viewer_uid {
            return false;
        }
        let visible_at = n.unlock_at.unwrap_or(n.at);
        now >= visible_at && now - visible_at <= 12 * 3600 * 1000
    });

    let mut uids: Vec<i64> = pet.logs.iter().map(|l| l.uid).filter(|u| *u > 0).collect();
    if let Some(q) = &pet.coop_quest {
        uids.push(q.step1_by);
    }

    let mut asset_ids: Vec<i64> = pet
        .souvenirs
        .iter()
        .filter_map(|s| s.ref_asset_id)
        .collect();
    if let Some(old_aid) = pet.current.carrying_old_asset_id {
        asset_ids.push(old_aid);
    }

    let (users, mut avatar_assets) = load_users_map(state, &uids).await?;
    asset_ids.append(&mut avatar_assets);
    let asset_urls = resolve_asset_urls(state, &asset_ids).await?;

    Ok(PetFullResp {
        skills: PetSkillsDto {
            can_paw_pick_menu: pet.adopted && pet.current.behavior != "ADVENTURING",
            has_unread_board_letter,
        },
        pet,
        shop_catalog: SHOP_CATALOG,
        sweet_resonance_triggered,
        users,
        asset_urls,
    })
}
