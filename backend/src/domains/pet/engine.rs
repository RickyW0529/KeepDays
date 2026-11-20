use rand::seq::SliceRandom;
use sqlx::Row;

use crate::{
    domains::{pet::model::{PetSouvenir, PetStateDoc}, timeline::record_timeline_event},
    infra::dict::dim,
    state::{AppState, PetLifeEvent, PetLifeTrigger},
    time_bj::{ambient_at, now_beijing, now_ms, AmbientPhase},
};

/// Lazy State Evaluation Engine (Zero idle disk I/O!):
/// Advances pet biological stats, settles completed adventures into Old-Photo Memory Postcards,
/// and transitions autonomous wall-roaming / fridge-sleeping behaviors anchored to Beijing Time (`UTC+8`).
pub async fn evaluate_pet_lazy_tick(
    state: &AppState,
    space_id: i64,
    pet: &mut PetStateDoc,
) -> bool {
    if !pet.adopted {
        return false;
    }

    let now = now_ms();
    if pet.last_tick_at == 0 {
        pet.last_tick_at = now;
        return true;
    }

    let elapsed_ms = (now - pet.last_tick_at).max(0);
    let mut dirty = false;

    // 1. Settle ongoing autonomous adventure if `return_at <= now`
    if pet.current.behavior == "ADVENTURING" {
        if let Some(ret_at) = pet.current.adventure_return_at {
            if now >= ret_at {
                let souvenir = generate_adventure_souvenir(state, space_id, &pet.name, now).await;
                pet.souvenirs.insert(0, souvenir.clone());
                pet.souvenirs.truncate(50);
                pet.current.behavior = "SUNBATHING_FRIDGE".to_string();
                pet.current.adventure_started_at = None;
                pet.current.adventure_return_at = None;
                pet.add_exp(35);
                pet.stats.bond += 15;
                pet.last_tick_at = now;

                // Resolve valid space owner_id for `timeline_events.actor_id` foreign key
                if let Ok(Some(s_row)) =
                    sqlx::query("SELECT owner_id FROM spaces WHERE id = ?1")
                        .bind(space_id)
                        .fetch_optional(&state.db.read)
                        .await
                {
                    let owner_id: i64 = s_row.get("owner_id");
                    let _ = record_timeline_event(
                        state,
                        space_id,
                        owner_id,
                        dim::EVT_PET_MOMENT,
                        serde_json::json!({
                            "pet_name": pet.name,
                            "autonomous": true,
                            "souvenir": souvenir,
                        }),
                    )
                    .await;
                }
                return true;
            } else {
                // Still adventuring outside
                return false;
            }
        }
    }

    // Expire stale co-op quest if past deadline
    if let Some(q) = &pet.coop_quest {
        if now > q.expires_at {
            pet.coop_quest = None;
            dirty = true;
        }
    }

    // 2. Biological drift (only ticks meaningfully every >= 10 minutes)
    if elapsed_ms >= 600_000 {
        let hours = (elapsed_ms as f64) / 3_600_000.0;
        let satiety_drop = (hours * 3.0).round() as u8;
        let mood_drop = (hours * 1.5).round() as u8;

        // Soft floors guarantee zero starvation / guilt!
        pet.stats.satiety = pet.stats.satiety.saturating_sub(satiety_drop).max(25);
        pet.stats.mood = pet.stats.mood.saturating_sub(mood_drop).max(40);

        // Long offline periods gently cultivate the "Independent Explorer" personality!
        if hours >= 18.0 {
            pet.traits.clingy_vs_independent = (pet.traits.clingy_vs_independent - 3).clamp(-100, 100);
        }

        let bj_now = now_beijing();
        let ambient = ambient_at(bj_now);

        match ambient.phase {
            AmbientPhase::Night => {
                // Nighttime in Beijing (19:30 - 07:00): Rest on fridge under warm nightlight & recover energy
                let energy_gain = (hours * 12.0).round() as u8;
                pet.stats.energy = (pet.stats.energy.saturating_add(energy_gain)).min(100);
                pet.current.behavior = "SLEEPING_FRIDGE".to_string();
                pet.current.focus_photo_id = None;
                pet.current.carrying_old_photo_id = None;
                pet.current.carrying_old_asset_id = None;
            }
            AmbientPhase::Dusk => {
                pet.stats.energy = (pet.stats.energy.saturating_add(5)).min(100);
                pet.current.behavior = "WATCHING_MENU".to_string();
                pet.current.carrying_old_photo_id = None;
                pet.current.carrying_old_asset_id = None;
            }
            AmbientPhase::Daylight => {
                // Daytime Markov autonomous transition along the Photo Wall!
                transition_daytime_behavior(state, space_id, pet).await;
            }
        }

        pet.last_tick_at = now;
        dirty = true;
    }

    dirty
}

async fn transition_daytime_behavior(state: &AppState, space_id: i64, pet: &mut PetStateDoc) {
    // Query up to 10 oldest & 10 newest photos in this space for wall roaming & memory awakening
    let old_rows = sqlx::query(
        "SELECT id, asset_id FROM photos WHERE space_id = ?1 ORDER BY taken_at ASC LIMIT 10",
    )
    .bind(space_id)
    .fetch_all(&state.db.read)
    .await
    .unwrap_or_default();

    let recent_rows = sqlx::query(
        "SELECT id FROM photos WHERE space_id = ?1 ORDER BY taken_at DESC LIMIT 12",
    )
    .bind(space_id)
    .fetch_all(&state.db.read)
    .await
    .unwrap_or_default();

    let owns_box = pet.unlocked_decor.iter().any(|d| d == "cardboard_box");
    let roll = (now_ms() % 100) as u8;

    if !old_rows.is_empty() && roll < 22 {
        // Evolution #2: Pet carries a sunken old photo in its mouth back to the fridge top!
        if let Some(chosen) = old_rows.choose(&mut rand::thread_rng()) {
            let pid: i64 = chosen.get("id");
            let aid: i64 = chosen.get("asset_id");
            pet.current.behavior = "CARRYING_OLD_PHOTO".to_string();
            pet.current.carrying_old_photo_id = Some(pid);
            pet.current.carrying_old_asset_id = Some(aid);
            pet.current.focus_photo_id = Some(pid);
            return;
        }
    }

    pet.current.carrying_old_photo_id = None;
    pet.current.carrying_old_asset_id = None;

    if !recent_rows.is_empty() && roll < 62 {
        // Walk slowly along the Photo Wall frames & gaze at a specific photo!
        if let Some(chosen) = recent_rows.choose(&mut rand::thread_rng()) {
            let pid: i64 = chosen.get("id");
            pet.current.behavior = if roll < 42 {
                "ROAMING_WALL".to_string()
            } else {
                "PEEKING_PHOTO".to_string()
            };
            pet.current.focus_photo_id = Some(pid);
            return;
        }
    }

    if owns_box && roll < 82 {
        pet.current.behavior = "PLAYING_BOX".to_string();
        pet.current.focus_photo_id = None;
    } else {
        pet.current.behavior = "SUNBATHING_FRIDGE".to_string();
        pet.current.focus_photo_id = None;
    }
}

pub async fn generate_adventure_souvenir(
    state: &AppState,
    space_id: i64,
    pet_name: &str,
    now: i64,
) -> PetSouvenir {
    // Try picking a real historical photo from this space to create a "Memory Postcard"!
    let rows = sqlx::query(
        "SELECT id, asset_id FROM photos WHERE space_id = ?1 ORDER BY RANDOM() LIMIT 1",
    )
    .bind(space_id)
    .fetch_all(&state.db.read)
    .await
    .unwrap_or_default();

    if let Some(r) = rows.first() {
        let pid: i64 = r.get("id");
        let aid: i64 = r.get("asset_id");
        return PetSouvenir {
            id: state.snowflake.next_id(),
            kind: "MEMORY_POSTCARD".to_string(),
            title: format!("{}的故地重游明信片", pet_name),
            ref_photo_id: Some(pid),
            ref_asset_id: Some(aid),
            diary: format!(
                "今天背着小包溜达去了你们以前拍照片的地方，蹲在相框角落吹了好久的风～"
            ),
            at: now,
        };
    }

    let gifts = [
        ("一片金黄银杏叶", "在街角长椅下捡到的书签，带回来夹在冰箱上啦。"),
        ("温润白鹅卵石", "在小溪边踩水时挑了一颗最圆的石头送给你们。"),
        ("松果小标本", "路过公园松树下掉在脑袋上的小礼物！"),
    ];
    let (title, diary) = gifts
        .choose(&mut rand::thread_rng())
        .copied()
        .unwrap_or(gifts[0]);

    PetSouvenir {
        id: state.snowflake.next_id(),
        kind: "NATURE_GIFT".to_string(),
        title: title.to_string(),
        ref_photo_id: None,
        ref_asset_id: None,
        diary: diary.to_string(),
        at: now,
    }
}

/// Asynchronous Domain Event Listener:
/// Nourishes the Shared Space Pet with Paw Coins & Personality Evolution whenever space members live their lives!
pub fn spawn_pet_life_listener(
    state: AppState,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<PetLifeEvent>,
) {
    tokio::spawn(async move {
        while let Some(evt) = rx.recv().await {
            let _ = apply_life_reward(&state, evt).await;
        }
    });
}

async fn apply_life_reward(state: &AppState, evt: PetLifeEvent) -> Result<(), sqlx::Error> {
    let mut tx = state.db.write.begin().await?;
    let row = sqlx::query(
        "SELECT data_json FROM space_modules WHERE space_id = ?1 AND module_type_id = ?2",
    )
    .bind(evt.space_id)
    .bind(dim::MOD_PET)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(r) = row else {
        return Ok(());
    };

    let Ok(mut pet) = serde_json::from_str::<PetStateDoc>(r.get("data_json")) else {
        return Ok(());
    };
    if !pet.adopted {
        // Still accumulate initial paw_coins even before adoption ceremony!
        pet.paw_coins += 10;
    } else {
        match evt.action {
            PetLifeTrigger::PhotoUploaded => {
                pet.paw_coins += 15;
                pet.add_exp(12);
                pet.traits.curiosity = (pet.traits.curiosity + 3).clamp(0, 100);
                pet.stats.mood = (pet.stats.mood.saturating_add(6)).min(100);
            }
            PetLifeTrigger::PhotoCommented => {
                pet.paw_coins += 5;
                pet.add_exp(5);
                pet.traits.curiosity = (pet.traits.curiosity + 1).clamp(0, 100);
            }
            PetLifeTrigger::MenuPinned => {
                pet.paw_coins += 20;
                pet.add_exp(15);
                // Smelling tonight's menu makes it happy & slightly fuller!
                pet.stats.satiety = (pet.stats.satiety.saturating_add(10)).min(100);
                pet.stats.mood = (pet.stats.mood.saturating_add(8)).min(100);
                pet.traits.foodie_vs_athletic = (pet.traits.foodie_vs_athletic + 2).clamp(-100, 100);
                if pet.current.behavior != "ADVENTURING" {
                    pet.current.behavior = "WATCHING_MENU".to_string();
                }
            }
            PetLifeTrigger::TodoCompleted => {
                pet.paw_coins += 10;
                pet.add_exp(10);
                pet.stats.bond += 5;
            }
        }
    }

    if let Ok(raw) = serde_json::to_string(&pet) {
        sqlx::query(
            "UPDATE space_modules SET data_json = ?1, version = version + 1, updated_at = ?2
             WHERE space_id = ?3 AND module_type_id = ?4",
        )
        .bind(&raw)
        .bind(now_ms())
        .bind(evt.space_id)
        .bind(dim::MOD_PET)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    state
        .hub
        .publish(
            evt.space_id,
            "PET_UPDATED",
            serde_json::to_value(&pet).unwrap_or_default(),
        )
        .await;

    Ok(())
}
