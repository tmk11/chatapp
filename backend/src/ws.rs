use crate::{error::AppError, messages::MessageView, state::AppState};
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, warn};
use uuid::Uuid;

const CHANNEL_CAPACITY: usize = 1024;
const MAX_FRAME_BYTES: usize = 8192;

/// Routes outbound events to every active WebSocket connection of a user.
#[derive(Clone, Default)]
pub struct UserHub {
    users: Arc<RwLock<HashMap<Uuid, broadcast::Sender<OutboundEvent>>>>,
}

impl UserHub {
    async fn register(&self, user_id: Uuid) -> broadcast::Sender<OutboundEvent> {
        let mut users = self.users.write().await;
        users
            .entry(user_id)
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .clone()
    }

    pub async fn send_to(&self, user_id: Uuid, event: OutboundEvent) {
        if let Some(tx) = self.users.read().await.get(&user_id) {
            let _ = tx.send(event);
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    token: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboundEvent {
    Message {
        message: MessageView,
    },
    MessageDeleted {
        message_id: Uuid,
        sender_id: Uuid,
        recipient_id: Uuid,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Deserialize)]
struct IncomingMessage {
    to: Uuid,
    body: Option<String>,
    attachment_id: Option<Uuid>,
}

/// GET /ws?token=<JWT> — one connection per user session. The client sends
/// `{"to":"<friend user id>","body":"..."}` frames for text, or
/// `{"to":"...","attachment_id":"..."}` frames referencing a previously
/// uploaded image; the server persists the message and fans it out to both
/// participants' connections. Sending is only allowed between friends.
pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let claims = state.auth.validate_token(&query.token)?;
    Ok(ws.on_upgrade(move |socket| handle_socket(state, socket, claims.sub)))
}

async fn handle_socket(state: AppState, mut socket: WebSocket, user_id: Uuid) {
    let tx = state.user_hub.register(user_id).await;
    let mut rx = tx.subscribe();
    debug!(%user_id, "websocket connected");

    loop {
        tokio::select! {
            maybe_incoming = socket.recv() => {
                match maybe_incoming {
                    Some(Ok(Message::Text(text))) => {
                        let reply = handle_incoming(&state, user_id, &text).await;
                        if let Some(event) = reply
                            && send_event(&mut socket, &event).await.is_err()
                        {
                            break;
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
                        if send_event(&mut socket, &event).await.is_err() {
                            break;
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
    debug!(%user_id, "websocket disconnected");
}

/// Validates, persists, and fans out one incoming frame. Returns an error
/// event to echo back to the sender's socket when the frame is rejected.
async fn handle_incoming(state: &AppState, user_id: Uuid, text: &str) -> Option<OutboundEvent> {
    if text.len() > MAX_FRAME_BYTES {
        return Some(OutboundEvent::Error {
            error: "message too large".to_owned(),
        });
    }
    let incoming = match serde_json::from_str::<IncomingMessage>(text) {
        Ok(incoming) => incoming,
        Err(_) => {
            return Some(OutboundEvent::Error {
                error: "invalid message".to_owned(),
            });
        }
    };
    if !state.friends.are_friends(user_id, incoming.to).await {
        return Some(OutboundEvent::Error {
            error: "recipient is not your friend".to_owned(),
        });
    }
    let stored = match (incoming.body, incoming.attachment_id) {
        (Some(body), None) => state.messages.append(user_id, incoming.to, body).await,
        (None, Some(attachment_id)) => {
            match state
                .attachments
                .attach(attachment_id, user_id, incoming.to)
                .await
            {
                Ok(()) => {
                    state
                        .messages
                        .append_image(user_id, incoming.to, attachment_id)
                        .await
                }
                Err(_) => {
                    return Some(OutboundEvent::Error {
                        error: "unknown or already used attachment".to_owned(),
                    });
                }
            }
        }
        _ => {
            return Some(OutboundEvent::Error {
                error: "message must contain either a body or an attachment_id".to_owned(),
            });
        }
    };
    let message = match stored {
        Ok(message) => message,
        Err(_) => {
            return Some(OutboundEvent::Error {
                error: "invalid message".to_owned(),
            });
        }
    };
    let recipient_id = message.recipient_id;
    let event = OutboundEvent::Message { message };
    state.user_hub.send_to(user_id, event.clone()).await;
    state.user_hub.send_to(recipient_id, event).await;
    None
}

async fn send_event(socket: &mut WebSocket, event: &OutboundEvent) -> Result<(), ()> {
    match serde_json::to_string(event) {
        Ok(json) => socket
            .send(Message::Text(json.into()))
            .await
            .map_err(|_| ()),
        Err(error) => {
            warn!(%error, "failed to serialize websocket event");
            Ok(())
        }
    }
}
