use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Custom epoch: 2025-01-01T00:00:00Z in milliseconds
const KEEPDAYS_EPOCH_MS: i64 = 1_735_689_600_000;

struct SnowflakeInner {
    last_ts_ms: i64,
    sequence: i64,
}

/// Thread-safe 64-bit Twitter Snowflake ID Generator (`i64`)
/// Layout: [0 (1 bit)] [timestamp_ms - epoch (41 bits)] [worker_id (10 bits)] [sequence (12 bits)]
/// Maps directly to SQLite native 64-bit `INTEGER PRIMARY KEY` (`rowid`).
pub struct SnowflakeGenerator {
    worker_id: i64,
    inner: Mutex<SnowflakeInner>,
}

impl SnowflakeGenerator {
    pub fn new(worker_id: u16) -> Self {
        Self {
            worker_id: (worker_id & 0x3FF) as i64,
            inner: Mutex::new(SnowflakeInner {
                last_ts_ms: 0,
                sequence: 0,
            }),
        }
    }

    pub fn next_id(&self) -> i64 {
        let mut guard = self.inner.lock().expect("snowflake mutex poisoned");
        let mut now = current_ms();

        if now < guard.last_ts_ms {
            // Clock drift protection: hold monotonic timestamp
            now = guard.last_ts_ms;
        }

        if now == guard.last_ts_ms {
            guard.sequence = (guard.sequence + 1) & 0xFFF;
            if guard.sequence == 0 {
                // Sequence exhausted in current ms, spin until next ms
                while now <= guard.last_ts_ms {
                    std::hint::spin_loop();
                    now = current_ms();
                }
            }
        } else {
            guard.sequence = 0;
        }

        guard.last_ts_ms = now;
        let delta = (now - KEEPDAYS_EPOCH_MS).max(0);
        (delta << 22) | (self.worker_id << 12) | guard.sequence
    }
}

fn current_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Twitter `id_str` Serde helper:
/// Serializes `i64` as JSON String `"7283918273..."` to protect JavaScript (> 2^53) precision,
/// while deserializing seamlessly from either JSON String or JSON Number.
pub mod i64_str {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use serde_json::Value;

    pub fn serialize<S>(val: &i64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&val.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Value::deserialize(deserializer)?;
        match v {
            Value::String(s) => s.parse::<i64>().map_err(serde::de::Error::custom),
            Value::Number(n) => n
                .as_i64()
                .ok_or_else(|| serde::de::Error::custom("Invalid i64 number")),
            _ => Err(serde::de::Error::custom(
                "Expected Snowflake ID string or integer",
            )),
        }
    }
}

pub mod opt_i64_str {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use serde_json::Value;

    pub fn serialize<S>(val: &Option<i64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match val {
            Some(v) => serializer.serialize_some(&v.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Option::<Value>::deserialize(deserializer)?;
        match v {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.is_empty() => Ok(None),
            Some(Value::String(s)) => s.parse::<i64>().map(Some).map_err(serde::de::Error::custom),
            Some(Value::Number(n)) => n
                .as_i64()
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("Invalid i64 number")),
            _ => Err(serde::de::Error::custom("Expected optional Snowflake ID")),
        }
    }
}

pub mod vec_i64_str {
    use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    pub fn serialize<S>(vals: &[i64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let strs: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
        strs.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let arr = Vec::<Value>::deserialize(deserializer)?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            match item {
                Value::String(s) => out.push(s.parse::<i64>().map_err(serde::de::Error::custom)?),
                Value::Number(n) => out.push(
                    n.as_i64()
                        .ok_or_else(|| serde::de::Error::custom("Invalid i64"))?,
                ),
                _ => return Err(serde::de::Error::custom("Invalid ID in array")),
            }
        }
        Ok(out)
    }
}
