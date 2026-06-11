use crate::{error::AppError, messages::MessageView, state::AppState};
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

const CHANNEL_CAPACITY: usize = 1024;
const MAX_FRAME_BYTES: usize = 8192;

#[derive(Default)]
struct HubEntry {
    sender: Option<broadcast::Sender<OutboundEvent>>,
    connections: usize,
}

/// Routes outbound events to every active WebSocket connection of a user and
/// tracks how many connections each user has for presence.
#[derive(Clone, Default)]
pub struct UserHub {
    users: Arc<RwLock<HashMap<Uuid, HubEntry>>>,
}

impl UserHub {
    /// Registers one connection. Returns the user's broadcast sender and
    /// whether this was the user's first concurrent connection.
    async fn connect(&self, user_id: Uuid) -> (broadcast::Sender<OutboundEvent>, bool) {
        let mut users = self.users.write().await;
        let entry = users.entry(user_id).or_default();
        entry.connections += 1;
        let first = entry.connections == 1;
        let sender = entry
            .sender
            .get_or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .clone();
        (sender, first)
    }

    /// Unregisters one connection. Returns true when it was the user's last.
    async fn disconnect(&self, user_id: Uuid) -> bool {
        let mut users = self.users.write().await;
        let Some(entry) = users.get_mut(&user_id) else {
            return false;
        };
        entry.connections = entry.connections.saturating_sub(1);
        entry.connections == 0
    }

    pub async fn is_online(&self, user_id: Uuid) -> bool {
        self.users
            .read()
            .await
            .get(&user_id)
            .is_some_and(|entry| entry.connections > 0)
    }

    pub async fn send_to(&self, user_id: Uuid, event: OutboundEvent) {
        if let Some(sender) = self
            .users
            .read()
            .await
            .get(&user_id)
            .and_then(|entry| entry.sender.as_ref())
        {
            let _ = sender.send(event);
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
    /// Sent to the original sender when the recipient acks delivery.
    Delivered {
        message_ids: Vec<Uuid>,
        by: Uuid,
    },
    /// Sent to the original sender when the recipient reads the conversation.
    Read {
        message_ids: Vec<Uuid>,
        by: Uuid,
    },
    Typing {
        from: Uuid,
    },
    Presence {
        user_id: Uuid,
        online: bool,
        last_seen_at: Option<DateTime<Utc>>,
    },
    Reaction {
        message_id: Uuid,
        user_id: Uuid,
        emoji: String,
        added: bool,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum IncomingFrame {
    /// Text (`body`) or image (`attachment_id`) message — exactly one of the
    /// two — optionally replying to an earlier message in the conversation.
    Message {
        to: Uuid,
        body: Option<String>,
        attachment_id: Option<Uuid>,
        reply_to: Option<Uuid>,
    },
    /// Recipient acks that it received these messages (e.g. via this socket).
    Delivered {
        message_ids: Vec<Uuid>,
    },
    /// Recipient opened the conversation with `peer_id`: everything unread
    /// from that peer becomes read.
    Read {
        peer_id: Uuid,
    },
    Typing {
        to: Uuid,
    },
    /// Toggles an emoji reaction on a message in one of the user's
    /// conversations.
    Reaction {
        message_id: Uuid,
        emoji: String,
    },
}

/// GET /ws?token=<JWT> — one connection per user session. All frames are
/// JSON-tagged with `type` (message, delivered, read, typing, reaction); see
/// `IncomingFrame` and `OutboundEvent`. Sending messages, typing signals, and
/// reactions is only allowed between friends.
pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, AppError> {
    let claims = state.auth.validate_token(&query.token)?;
    Ok(ws.on_upgrade(move |socket| handle_socket(state, socket, claims.sub)))
}

async fn handle_socket(state: AppState, mut socket: WebSocket, user_id: Uuid) {
    let (sender, first_connection) = state.user_hub.connect(user_id).await;
    let mut receiver = sender.subscribe();
    debug!(%user_id, "websocket connected");
    if first_connection {
        broadcast_presence(&state, user_id, true, None).await;
    }

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
            event = receiver.recv() => {
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

    if state.user_hub.disconnect(user_id).await {
        let now = Utc::now();
        state.users.set_last_seen(user_id, now).await;
        broadcast_presence(&state, user_id, false, Some(now)).await;
    }
    debug!(%user_id, "websocket disconnected");
}

/// Notifies all of the user's friends about an online/offline transition.
async fn broadcast_presence(
    state: &AppState,
    user_id: Uuid,
    online: bool,
    last_seen_at: Option<DateTime<Utc>>,
) {
    let friend_ids = match state.friends.friends_of(user_id).await {
        Ok(friend_ids) => friend_ids,
        Err(_) => return,
    };
    let event = OutboundEvent::Presence {
        user_id,
        online,
        last_seen_at,
    };
    for friend_id in friend_ids {
        state.user_hub.send_to(friend_id, event.clone()).await;
    }
}

/// Validates, persists, and fans out one incoming frame. Returns an error
/// event to echo back to the sender's socket when the frame is rejected.
async fn handle_incoming(state: &AppState, user_id: Uuid, text: &str) -> Option<OutboundEvent> {
    if text.len() > MAX_FRAME_BYTES {
        return error_event("message too large");
    }
    let frame = match serde_json::from_str::<IncomingFrame>(text) {
        Ok(frame) => frame,
        Err(_) => return error_event("invalid frame"),
    };

    match frame {
        IncomingFrame::Message {
            to,
            body,
            attachment_id,
            reply_to,
        } => handle_message(state, user_id, to, body, attachment_id, reply_to).await,
        IncomingFrame::Delivered { message_ids } => {
            handle_delivered(state, user_id, &message_ids).await
        }
        IncomingFrame::Read { peer_id } => handle_read(state, user_id, peer_id).await,
        IncomingFrame::Typing { to } => handle_typing(state, user_id, to).await,
        IncomingFrame::Reaction { message_id, emoji } => {
            handle_reaction(state, user_id, message_id, &emoji).await
        }
    }
}

async fn handle_message(
    state: &AppState,
    user_id: Uuid,
    to: Uuid,
    body: Option<String>,
    attachment_id: Option<Uuid>,
    reply_to: Option<Uuid>,
) -> Option<OutboundEvent> {
    match state.friends.are_friends(user_id, to).await {
        Ok(true) => {}
        Ok(false) => return error_event("recipient is not your friend"),
        Err(_) => return error_event("internal error"),
    }
    let stored = match (body, attachment_id) {
        (Some(body), None) => {
            state
                .messages
                .append_text(user_id, to, body, reply_to)
                .await
        }
        (None, Some(attachment_id)) => {
            match state.attachments.attach(attachment_id, user_id, to).await {
                Ok(()) => {
                    state
                        .messages
                        .append_image(user_id, to, attachment_id, reply_to)
                        .await
                }
                Err(_) => return error_event("unknown or already used attachment"),
            }
        }
        _ => return error_event("message must contain either a body or an attachment_id"),
    };
    let message = match stored {
        Ok(message) => message,
        Err(AppError::BadRequest(reason)) => return error_event(&reason),
        Err(_) => return error_event("invalid message"),
    };
    let recipient_id = message.recipient_id;
    let event = OutboundEvent::Message { message };
    state.user_hub.send_to(user_id, event.clone()).await;
    state.user_hub.send_to(recipient_id, event).await;
    None
}

async fn handle_delivered(
    state: &AppState,
    user_id: Uuid,
    message_ids: &[Uuid],
) -> Option<OutboundEvent> {
    let updated = match state.messages.mark_delivered(message_ids, user_id).await {
        Ok(updated) => updated,
        Err(_) => return error_event("internal error"),
    };
    // Group acked ids by original sender so each sender gets one event.
    let mut by_sender: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (message_id, sender_id) in updated {
        by_sender.entry(sender_id).or_default().push(message_id);
    }
    for (sender_id, message_ids) in by_sender {
        state
            .user_hub
            .send_to(
                sender_id,
                OutboundEvent::Delivered {
                    message_ids,
                    by: user_id,
                },
            )
            .await;
    }
    None
}

async fn handle_read(state: &AppState, user_id: Uuid, peer_id: Uuid) -> Option<OutboundEvent> {
    let message_ids = match state.messages.mark_read(user_id, peer_id).await {
        Ok(message_ids) => message_ids,
        Err(_) => return error_event("internal error"),
    };
    if !message_ids.is_empty() {
        state
            .user_hub
            .send_to(
                peer_id,
                OutboundEvent::Read {
                    message_ids,
                    by: user_id,
                },
            )
            .await;
    }
    None
}

async fn handle_typing(state: &AppState, user_id: Uuid, to: Uuid) -> Option<OutboundEvent> {
    match state.friends.are_friends(user_id, to).await {
        Ok(true) => {
            state
                .user_hub
                .send_to(to, OutboundEvent::Typing { from: user_id })
                .await;
            None
        }
        Ok(false) => error_event("recipient is not your friend"),
        Err(_) => error_event("internal error"),
    }
}

async fn handle_reaction(
    state: &AppState,
    user_id: Uuid,
    message_id: Uuid,
    emoji: &str,
) -> Option<OutboundEvent> {
    let toggle = match state
        .messages
        .toggle_reaction(message_id, user_id, emoji)
        .await
    {
        Ok(toggle) => toggle,
        Err(AppError::BadRequest(reason)) => return error_event(&reason),
        Err(_) => return error_event("unknown message"),
    };
    let event = OutboundEvent::Reaction {
        message_id,
        user_id,
        emoji: emoji.to_owned(),
        added: toggle.added,
    };
    state
        .user_hub
        .send_to(toggle.sender_id, event.clone())
        .await;
    state.user_hub.send_to(toggle.recipient_id, event).await;
    None
}

fn error_event(message: &str) -> Option<OutboundEvent> {
    Some(OutboundEvent::Error {
        error: message.to_owned(),
    })
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
