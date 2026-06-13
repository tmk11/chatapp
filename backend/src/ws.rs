use crate::{
    chat::{MessageKind, MessageView, NewMessage},
    error::AppError,
    state::AppState,
};
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
        conversation_id: Uuid,
        message_id: Uuid,
    },
    MessagePinned {
        conversation_id: Uuid,
        message_id: Uuid,
        pinned: bool,
    },
    /// A member advanced their read cursor in a conversation.
    Read {
        conversation_id: Uuid,
        user_id: Uuid,
        at: DateTime<Utc>,
    },
    Typing {
        conversation_id: Uuid,
        from: Uuid,
    },
    Presence {
        user_id: Uuid,
        online: bool,
        last_seen_at: Option<DateTime<Utc>>,
    },
    Reaction {
        conversation_id: Uuid,
        message_id: Uuid,
        user_id: Uuid,
        emoji: String,
        added: bool,
    },
    /// Membership or metadata changed; clients should refetch the list.
    ConversationUpdated {
        conversation_id: Uuid,
    },
    Error {
        error: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum IncomingFrame {
    /// Sends a message to a conversation the user belongs to. `kind` defaults
    /// to text; image/voice carry an `attachment_id` (voice also `duration_ms`).
    Message {
        conversation_id: Uuid,
        #[serde(default)]
        kind: IncomingKind,
        body: Option<String>,
        attachment_id: Option<Uuid>,
        duration_ms: Option<i32>,
        reply_to: Option<Uuid>,
    },
    /// The user opened/read a conversation up to now.
    Read {
        conversation_id: Uuid,
    },
    Typing {
        conversation_id: Uuid,
    },
    Reaction {
        message_id: Uuid,
        emoji: String,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IncomingKind {
    #[default]
    Text,
    Image,
    Voice,
}

impl IncomingKind {
    fn to_kind(&self) -> MessageKind {
        match self {
            IncomingKind::Text => MessageKind::Text,
            IncomingKind::Image => MessageKind::Image,
            IncomingKind::Voice => MessageKind::Voice,
        }
    }
}

/// GET /ws?token=<JWT> — one connection per user session. All frames are
/// JSON-tagged with `type` (message, read, typing, reaction). Membership is
/// enforced per conversation.
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

async fn fan_out(state: &AppState, conversation_id: Uuid, event: OutboundEvent) {
    let Ok(member_ids) = state.chat.members(conversation_id).await else {
        return;
    };
    for member_id in member_ids {
        state.user_hub.send_to(member_id, event.clone()).await;
    }
}

fn error_event(message: &str) -> Option<OutboundEvent> {
    Some(OutboundEvent::Error {
        error: message.to_owned(),
    })
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
            conversation_id,
            kind,
            body,
            attachment_id,
            duration_ms,
            reply_to,
        } => {
            handle_message(
                state,
                user_id,
                conversation_id,
                kind.to_kind(),
                body,
                attachment_id,
                duration_ms,
                reply_to,
            )
            .await
        }
        IncomingFrame::Read { conversation_id } => {
            handle_read(state, user_id, conversation_id).await
        }
        IncomingFrame::Typing { conversation_id } => {
            handle_typing(state, user_id, conversation_id).await
        }
        IncomingFrame::Reaction { message_id, emoji } => {
            handle_reaction(state, user_id, message_id, &emoji).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_message(
    state: &AppState,
    user_id: Uuid,
    conversation_id: Uuid,
    kind: MessageKind,
    body: Option<String>,
    attachment_id: Option<Uuid>,
    duration_ms: Option<i32>,
    reply_to: Option<Uuid>,
) -> Option<OutboundEvent> {
    match state.chat.is_member(conversation_id, user_id).await {
        Ok(true) => {}
        Ok(false) => return error_event("you are not in this conversation"),
        Err(_) => return error_event("internal error"),
    }

    // Media must be a fresh attachment owned by the sender; mark it used.
    if matches!(kind, MessageKind::Image | MessageKind::Voice) {
        let Some(attachment_id) = attachment_id else {
            return error_event("attachment_id required");
        };
        if state
            .attachments
            .mark_used(attachment_id, user_id)
            .await
            .is_err()
        {
            return error_event("unknown or already used attachment");
        }
    }

    let draft = NewMessage {
        kind,
        body: body.unwrap_or_default(),
        attachment_id: if matches!(kind, MessageKind::Image | MessageKind::Voice) {
            attachment_id
        } else {
            None
        },
        duration_ms,
        reply_to,
    };

    let message = match state.chat.append(conversation_id, user_id, draft).await {
        Ok(message) => message,
        Err(AppError::BadRequest(reason)) => return error_event(&reason),
        Err(_) => return error_event("could not send message"),
    };
    fan_out(state, conversation_id, OutboundEvent::Message { message }).await;
    None
}

async fn handle_read(
    state: &AppState,
    user_id: Uuid,
    conversation_id: Uuid,
) -> Option<OutboundEvent> {
    match state.chat.mark_read(conversation_id, user_id).await {
        Ok(at) => {
            fan_out(
                state,
                conversation_id,
                OutboundEvent::Read {
                    conversation_id,
                    user_id,
                    at,
                },
            )
            .await;
            None
        }
        Err(AppError::Forbidden) => error_event("you are not in this conversation"),
        Err(_) => error_event("internal error"),
    }
}

async fn handle_typing(
    state: &AppState,
    user_id: Uuid,
    conversation_id: Uuid,
) -> Option<OutboundEvent> {
    match state.chat.members(conversation_id).await {
        Ok(member_ids) if member_ids.contains(&user_id) => {
            let event = OutboundEvent::Typing {
                conversation_id,
                from: user_id,
            };
            for member_id in member_ids {
                if member_id != user_id {
                    state.user_hub.send_to(member_id, event.clone()).await;
                }
            }
            None
        }
        Ok(_) => error_event("you are not in this conversation"),
        Err(_) => error_event("internal error"),
    }
}

async fn handle_reaction(
    state: &AppState,
    user_id: Uuid,
    message_id: Uuid,
    emoji: &str,
) -> Option<OutboundEvent> {
    let result = match state.chat.toggle_reaction(message_id, user_id, emoji).await {
        Ok(result) => result,
        Err(AppError::BadRequest(reason)) => return error_event(&reason),
        Err(_) => return error_event("unknown message"),
    };
    let conversation_id = result.view.conversation_id;
    fan_out(
        state,
        conversation_id,
        OutboundEvent::Reaction {
            conversation_id,
            message_id,
            user_id,
            emoji: result.emoji,
            added: result.added,
        },
    )
    .await;
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
