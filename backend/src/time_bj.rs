use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Asia::Shanghai;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// Global Beijing Timezone (`UTC+8 / Asia/Shanghai`)
pub fn now_beijing() -> DateTime<Tz> {
    Utc::now().with_timezone(&Shanghai)
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Museum-grade zero-particle Ambient Lighting Phase anchored to Beijing Time (`UTC+8`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AmbientPhase {
    /// 07:00 - 17:30: Warm Gallery Off-White (#FAF9F6), crisp natural long shadow
    Daylight,
    /// 17:30 - 19:30: Subtle Golden Dusk warmth
    Dusk,
    /// 19:30 - 07:00: Obsidian Dark Gallery (#0E0E10) + Mini Fridge warm breathing nightlight
    Night,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmbientStatus {
    pub timezone: &'static str,
    pub beijing_iso: String,
    pub date_label: String,
    pub hour: u32,
    pub minute: u32,
    pub phase: AmbientPhase,
    pub fridge_nightlight_on: bool,
    pub canvas_bg_hex: &'static str,
}

pub fn current_ambient_status() -> AmbientStatus {
    ambient_at(now_beijing())
}

pub fn ambient_at(dt: DateTime<Tz>) -> AmbientStatus {
    let h = dt.hour();
    let m = dt.minute();
    let minutes_of_day = h * 60 + m;

    let phase = if (7 * 60..17 * 60 + 30).contains(&minutes_of_day) {
        AmbientPhase::Daylight
    } else if (17 * 60 + 30..19 * 60 + 30).contains(&minutes_of_day) {
        AmbientPhase::Dusk
    } else {
        AmbientPhase::Night
    };

    let (fridge_nightlight_on, canvas_bg_hex) = match phase {
        AmbientPhase::Daylight => (false, "#FAF9F6"),
        AmbientPhase::Dusk => (false, "#F5EFE8"),
        AmbientPhase::Night => (true, "#0E0E10"),
    };

    AmbientStatus {
        timezone: "Asia/Shanghai",
        beijing_iso: dt.to_rfc3339(),
        date_label: dt.format("%m.%d").to_string(),
        hour: h,
        minute: m,
        phase,
        fridge_nightlight_on,
        canvas_bg_hex,
    }
}
