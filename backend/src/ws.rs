use crate::{error::AppError, state::AppState};
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, warn};
use uuid::Uuid;

const ROOM_CAPACITY: usize = 1024;
const MAX_MESSAGE_BYTES: usize = 4096;

#[derive(Clone, Default)]
pub struct RoomHub {
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
}

impl RoomHub {
    async fn join(&self, room_id: &str) -> broadcast::Sender<ChatEvent> {
        let mut rooms = self.rooms.write().await;
        rooms
            .entry(room_id.to_owned())
            .or_insert_with(|| broadcast::channel(ROOM_CAPACITY).0)
            .clone()
    }
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    room_id: String,
    token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatEvent {
    id: Uuid,
    room_id: String,
    sender_id: Uuid,
    body: String,
    sent_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct IncomingMessage {
    body: String,
}

pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let claims = state.auth.validate_token(&query.token)?;
    let room_id = query.room_id.trim().to_owned();
    if room_id.is_empty() || room_id.len() > 128 {
        return Err(AppError::BadRequest("invalid room id".to_owned()));
    }
    if state.rooms.find_by_id(&room_id).await.is_none() {
        return Err(AppError::BadRequest("unknown room id".to_owned()));
    }

    Ok(ws.on_upgrade(move |socket| handle_socket(state, socket, room_id, claims.sub)))
}

async fn handle_socket(state: AppState, mut socket: WebSocket, room_id: String, user_id: Uuid) {
    let tx = state.room_hub.join(&room_id).await;
    let mut rx = tx.subscribe();
    debug!(%room_id, %user_id, "websocket connected");

    loop {
        tokio::select! {
            maybe_incoming = socket.recv() => {
                match maybe_incoming {
                    Some(Ok(Message::Text(text))) => {
                        if text.len() > MAX_MESSAGE_BYTES {
                            let _ = socket.send(Message::Text("{\"error\":\"message too large\"}".into())).await;
                            continue;
                        }
                        match serde_json::from_str::<IncomingMessage>(&text) {
                            Ok(incoming) if !incoming.body.trim().is_empty() => {
                                let event = ChatEvent {
                                    id: Uuid::new_v4(),
                                    room_id: room_id.clone(),
                                    sender_id: user_id,
                                    body: incoming.body,
                                    sent_at: Utc::now(),
                                };
                                let _ = tx.send(event);
                            }
                            _ => {
                                let _ = socket.send(Message::Text("{\"error\":\"invalid message\"}".into())).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        warn!(%error, "websocket receive error");
                        break;
                    }
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        match serde_json::to_string(&event) {
                            Ok(json) => {
                                if socket.send(Message::Text(json.into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(error) => warn!(%error, "failed to serialize chat event"),
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "websocket receiver lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    debug!(%room_id, %user_id, "websocket disconnected");
}
