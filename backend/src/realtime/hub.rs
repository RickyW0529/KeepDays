use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::BroadcastStream;

use crate::{snowflake::i64_str, time_bj::now_ms};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeMessage {
    #[serde(with = "i64_str")]
    pub space_id: i64,
    /// "TIMELINE_NEW" | "PRESENCE_SYNC" | "FRIDGE_UPDATED" | "BOARD_UPDATED" | "PET_UPDATED"
    pub kind: String,
    pub payload: serde_json::Value,
    pub ts_ms: i64,
}

#[derive(Clone, Default)]
pub struct RealtimeHub {
    rooms: Arc<RwLock<HashMap<i64, broadcast::Sender<RealtimeMessage>>>>,
}

impl RealtimeHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn subscribe_space(&self, space_id: i64) -> broadcast::Receiver<RealtimeMessage> {
        let mut guard = self.rooms.write().await;
        let tx = guard.entry(space_id).or_insert_with(|| {
            let (tx, _) = broadcast::channel(128);
            tx
        });
        tx.subscribe()
    }

    pub async fn publish(&self, space_id: i64, kind: &str, payload: serde_json::Value) {
        let guard = self.rooms.read().await;
        if let Some(tx) = guard.get(&space_id) {
            let _ = tx.send(RealtimeMessage {
                space_id,
                kind: kind.to_string(),
                payload,
                ts_ms: now_ms(),
            });
        }
    }

    /// Drops SSE broadcast channels with 0 active subscribers to keep memory footprint tiny
    pub async fn prune_idle_rooms(&self) {
        let mut guard = self.rooms.write().await;
        guard.retain(|_, tx| tx.receiver_count() > 0);
        guard.shrink_to_fit();
    }

    pub async fn into_sse_stream(
        &self,
        space_id: i64,
        initial_presence: serde_json::Value,
    ) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
        let rx = self.subscribe_space(space_id).await;

        let init_msg = RealtimeMessage {
            space_id,
            kind: "CONNECTED".to_string(),
            payload: initial_presence,
            ts_ms: now_ms(),
        };
        let init_event = Event::default()
            .event("CONNECTED")
            .data(serde_json::to_string(&init_msg).unwrap_or_default());

        let init_stream = futures_util::stream::once(async move { Ok(init_event) });

        let live_stream = BroadcastStream::new(rx).filter_map(move |res| async move {
            match res {
                Ok(msg) => {
                    let json = serde_json::to_string(&msg).ok()?;
                    Some(Ok(Event::default().event(msg.kind).data(json)))
                }
                Err(_) => {
                    let resync = RealtimeMessage {
                        space_id,
                        kind: "RESYNC".to_string(),
                        payload: serde_json::json!({}),
                        ts_ms: now_ms(),
                    };
                    let json = serde_json::to_string(&resync).ok()?;
                    Some(Ok(Event::default().event("RESYNC").data(json)))
                }
            }
        });

        Sse::new(init_stream.chain(live_stream)).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(20))
                .text("keepalive"),
        )
    }
}
