use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::snowflake::{i64_str, opt_i64_str};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetStats {
    /// 20..=100 (soft floor at 20 — never starves or dies!)
    pub satiety: u8,
    /// 15..=100
    pub energy: u8,
    /// 30..=100
    pub mood: u8,
    /// Cumulative shared intimacy bond
    pub bond: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PetTraits {
    /// +100 = Clingy (粘人精), -100 = Independent Explorer (独行侠)
    pub clingy_vs_independent: i32,
    /// +100 = Foodie (小吃货), -100 = Athletic (运动健将)
    pub foodie_vs_athletic: i32,
    /// 0..=100 Curiosity (好奇宝宝)
    pub curiosity: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetCurrentState {
    /// "SLEEPING_FRIDGE" | "SUNBATHING_FRIDGE" | "ROAMING_WALL" | "PEEKING_PHOTO"
    /// | "CARRYING_OLD_PHOTO" | "WATCHING_MENU" | "PLAYING_BOX" | "ADVENTURING"
    pub behavior: String,
    #[serde(default, with = "opt_i64_str")]
    pub focus_photo_id: Option<i64>,
    #[serde(default, with = "opt_i64_str")]
    pub carrying_old_photo_id: Option<i64>,
    #[serde(default, with = "opt_i64_str")]
    pub carrying_old_asset_id: Option<i64>,
    #[serde(default)]
    pub adventure_started_at: Option<i64>,
    #[serde(default)]
    pub adventure_return_at: Option<i64>,
    #[serde(default)]
    pub equipped_accessory: Option<String>,
    pub bed_style: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoopQuest {
    pub quest_id: String,
    pub title: String,
    #[serde(with = "i64_str")]
    pub step1_by: i64,
    pub step1_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetSouvenir {
    #[serde(with = "i64_str")]
    pub id: i64,
    /// "MEMORY_POSTCARD" | "NATURE_GIFT"
    pub kind: String,
    pub title: String,
    #[serde(default, with = "opt_i64_str")]
    pub ref_photo_id: Option<i64>,
    #[serde(default, with = "opt_i64_str")]
    pub ref_asset_id: Option<i64>,
    pub diary: String,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetLogEntry {
    #[serde(with = "i64_str")]
    pub uid: i64,
    pub action: String,
    pub text: String,
    #[serde(default)]
    pub resonance: bool,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetStateDoc {
    pub adopted: bool,
    pub name: String,
    /// "BLACK_CAT" | "SHIBA" | "OTTER" | "BUNNY"
    pub species: String,
    pub stage: String,
    pub level: u32,
    pub exp: u32,
    pub paw_coins: i64,
    pub stats: PetStats,
    pub traits: PetTraits,
    pub current: PetCurrentState,
    #[serde(default)]
    pub inventory: HashMap<String, u32>,
    #[serde(default)]
    pub unlocked_decor: Vec<String>,
    #[serde(default)]
    pub coop_quest: Option<CoopQuest>,
    #[serde(default)]
    pub souvenirs: Vec<PetSouvenir>,
    #[serde(default)]
    pub logs: Vec<PetLogEntry>,
    pub adopted_at: Option<i64>,
    pub last_tick_at: i64,
}

impl Default for PetStateDoc {
    fn default() -> Self {
        Self::unadopted()
    }
}

impl PetStateDoc {
    pub fn unadopted() -> Self {
        Self {
            adopted: false,
            name: String::new(),
            species: "BLACK_CAT".to_string(),
            stage: "BABY".to_string(),
            level: 1,
            exp: 0,
            paw_coins: 60,
            stats: PetStats {
                satiety: 85,
                energy: 90,
                mood: 90,
                bond: 0,
            },
            traits: PetTraits::default(),
            current: PetCurrentState {
                behavior: "SLEEPING_FRIDGE".to_string(),
                focus_photo_id: None,
                carrying_old_photo_id: None,
                carrying_old_asset_id: None,
                adventure_started_at: None,
                adventure_return_at: None,
                equipped_accessory: None,
                bed_style: "linen_pad".to_string(),
            },
            inventory: HashMap::from([
                ("salmon_freeze_dried".to_string(), 2),
                ("duo_feast_can".to_string(), 1),
            ]),
            unlocked_decor: vec!["linen_pad".to_string()],
            coop_quest: None,
            souvenirs: Vec::new(),
            logs: Vec::new(),
            adopted_at: None,
            last_tick_at: 0,
        }
    }

    pub fn add_exp(&mut self, delta: u32) {
        self.exp += delta;
        let new_level = (self.exp / 100) + 1;
        if new_level > self.level {
            self.level = new_level;
        }
        self.stage = match self.level {
            1..=5 => "BABY",
            6..=15 => "GROWING",
            16..=29 => "MATURE",
            _ => "GUARDIAN",
        }
        .to_string();
    }

    pub fn push_log(&mut self, entry: PetLogEntry) {
        self.logs.insert(0, entry);
        self.logs.truncate(30);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ShopItemDef {
    pub item_id: &'static str,
    pub name: &'static str,
    /// "FOOD" | "DUO_FOOD" | "BED" | "ACCESSORY"
    pub category: &'static str,
    pub price_coins: i64,
    pub satiety_gain: u8,
    pub mood_gain: u8,
    pub energy_gain: u8,
    pub desc: &'static str,
}

pub const SHOP_CATALOG: &[ShopItemDef] = &[
    ShopItemDef {
        item_id: "salmon_freeze_dried",
        name: "三文鱼冻干",
        category: "FOOD",
        price_coins: 15,
        satiety_gain: 18,
        mood_gain: 12,
        energy_gain: 5,
        desc: "咔嚓酥脆，吃货倾向微增",
    },
    ShopItemDef {
        item_id: "warm_goat_milk",
        name: "温热羊奶",
        category: "FOOD",
        price_coins: 12,
        satiety_gain: 10,
        mood_gain: 8,
        energy_gain: 22,
        desc: "快速恢复探险与散步精力",
    },
    ShopItemDef {
        item_id: "duo_feast_can",
        name: "双人合喂鲜肉双拼罐",
        category: "DUO_FOOD",
        price_coins: 30,
        satiety_gain: 35,
        mood_gain: 30,
        energy_gain: 20,
        desc: "需一人开罐、另一人添勺合力完成，羁绊暴涨",
    },
    ShopItemDef {
        item_id: "cardboard_box",
        name: "极简瓦楞纸箱窝",
        category: "BED",
        price_coins: 50,
        satiety_gain: 0,
        mood_gain: 15,
        energy_gain: 0,
        desc: "放在右下角冰箱顶上，它总爱钻进去只露两只耳朵",
    },
    ShopItemDef {
        item_id: "rattan_nest",
        name: "手作藤编暖窝",
        category: "BED",
        price_coins: 80,
        satiety_gain: 0,
        mood_gain: 20,
        energy_gain: 0,
        desc: "透气自然肌理的冰箱顶窝垫",
    },
    ShopItemDef {
        item_id: "red_scarf",
        name: "陶土红小围巾",
        category: "ACCESSORY",
        price_coins: 60,
        satiety_gain: 0,
        mood_gain: 15,
        energy_gain: 0,
        desc: "极简一抹暖红点缀在黑色剪影脖颈间",
    },
    ShopItemDef {
        item_id: "straw_hat",
        name: "夏日微缩草帽",
        category: "ACCESSORY",
        price_coins: 65,
        satiety_gain: 0,
        mood_gain: 15,
        energy_gain: 0,
        desc: "外出探险散步时的可爱行头",
    },
];

pub fn find_shop_item(item_id: &str) -> Option<&'static ShopItemDef> {
    SHOP_CATALOG.iter().find(|i| i.item_id == item_id)
}
