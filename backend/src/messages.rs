use crate::{auth, error::AppError, state::AppState, ws::OutboundEvent};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;
use uuid::Uuid;

pub const MAX_MESSAGE_BODY_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Text,
    Image,
}

#[derive(Clone, Debug)]
struct StoredMessage {
    id: Uuid,
    sender_id: Uuid,
    recipient_id: Uuid,
    kind: MessageKind,
    body: String,
    attachment_id: Option<Uuid>,
    sent_at: DateTime<Utc>,
    deleted_for_everyone: bool,
    deleted_for: HashSet<Uuid>,
}

/// What a participant sees. Messages deleted for everyone keep their place in
/// the conversation as an empty tombstone; messages deleted only for the
/// requesting user are omitted entirely from their history.
#[derive(Clone, Debug, Serialize)]
pub struct MessageView {
    pub id: Uuid,
    pub sender_id: Uuid,
    pub recipient_id: Uuid,
    pub kind: MessageKind,
    pub body: String,
    pub attachment_id: Option<Uuid>,
    pub sent_at: DateTime<Utc>,
    pub deleted: bool,
}

impl StoredMessage {
    fn view(&self) -> MessageView {
        MessageView {
            id: self.id,
            sender_id: self.sender_id,
            recipient_id: self.recipient_id,
            kind: self.kind,
            body: if self.deleted_for_everyone {
                String::new()
            } else {
                self.body.clone()
            },
            attachment_id: if self.deleted_for_everyone {
                None
            } else {
                self.attachment_id
            },
            sent_at: self.sent_at,
            deleted: self.deleted_for_everyone,
        }
    }
}

type ConversationKey = (Uuid, Uuid);

fn conversation_key(a: Uuid, b: Uuid) -> ConversationKey {
    if a <= b { (a, b) } else { (b, a) }
}

#[derive(Clone, Default)]
pub struct InMemoryMessageStore {
    conversations: Arc<RwLock<HashMap<ConversationKey, Vec<StoredMessage>>>>,
    message_index: Arc<RwLock<HashMap<Uuid, ConversationKey>>>,
}

impl InMemoryMessageStore {
    pub async fn append(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        body: String,
    ) -> Result<MessageView, AppError> {
        let body = validate_body(&body)?;
        self.push(StoredMessage {
            id: Uuid::new_v4(),
            sender_id,
            recipient_id,
            kind: MessageKind::Text,
            body,
            attachment_id: None,
            sent_at: Utc::now(),
            deleted_for_everyone: false,
            deleted_for: HashSet::new(),
        })
        .await
    }

    /// The attachment must already be validated and bound to this sender and
    /// recipient via the attachment store before calling this.
    pub async fn append_image(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<MessageView, AppError> {
        self.push(StoredMessage {
            id: Uuid::new_v4(),
            sender_id,
            recipient_id,
            kind: MessageKind::Image,
            body: String::new(),
            attachment_id: Some(attachment_id),
            sent_at: Utc::now(),
            deleted_for_everyone: false,
            deleted_for: HashSet::new(),
        })
        .await
    }

    async fn push(&self, message: StoredMessage) -> Result<MessageView, AppError> {
        let view = message.view();
        let key = conversation_key(message.sender_id, message.recipient_id);
        self.message_index.write().await.insert(message.id, key);
        self.conversations
            .write()
            .await
            .entry(key)
            .or_default()
            .push(message);
        Ok(view)
    }

    pub async fn history(&self, user_id: Uuid, peer_id: Uuid) -> Vec<MessageView> {
        let key = conversation_key(user_id, peer_id);
        self.conversations
            .read()
            .await
            .get(&key)
            .map(|messages| {
                messages
                    .iter()
                    .filter(|message| !message.deleted_for.contains(&user_id))
                    .map(StoredMessage::view)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Hides the message from `user_id` only. Any conversation participant may
    /// delete any message for themselves.
    pub async fn delete_for_me(&self, message_id: Uuid, user_id: Uuid) -> Result<(), AppError> {
        let key = self.lookup_key(message_id).await?;
        if key.0 != user_id && key.1 != user_id {
            return Err(AppError::NotFound);
        }
        let mut conversations = self.conversations.write().await;
        let message = conversations
            .get_mut(&key)
            .and_then(|messages| messages.iter_mut().find(|message| message.id == message_id))
            .ok_or(AppError::NotFound)?;
        message.deleted_for.insert(user_id);
        Ok(())
    }

    /// Replaces the message with a tombstone for both participants. Only the
    /// original sender may delete a message for everyone. Returns the
    /// tombstone view plus the id of the attachment to purge, if any.
    pub async fn delete_for_everyone(
        &self,
        message_id: Uuid,
        user_id: Uuid,
    ) -> Result<(MessageView, Option<Uuid>), AppError> {
        let key = self.lookup_key(message_id).await?;
        if key.0 != user_id && key.1 != user_id {
            return Err(AppError::NotFound);
        }
        let mut conversations = self.conversations.write().await;
        let message = conversations
            .get_mut(&key)
            .and_then(|messages| messages.iter_mut().find(|message| message.id == message_id))
            .ok_or(AppError::NotFound)?;
        if message.sender_id != user_id {
            return Err(AppError::Forbidden);
        }
        message.deleted_for_everyone = true;
        message.body.clear();
        let purged_attachment = message.attachment_id.take();
        Ok((message.view(), purged_attachment))
    }

    async fn lookup_key(&self, message_id: Uuid) -> Result<(Uuid, Uuid), AppError> {
        self.message_index
            .read()
            .await
            .get(&message_id)
            .copied()
            .ok_or(AppError::NotFound)
    }
}

fn validate_body(body: &str) -> Result<String, AppError> {
    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_MESSAGE_BODY_BYTES {
        return Err(AppError::BadRequest(format!(
            "message body must be 1-{MAX_MESSAGE_BODY_BYTES} bytes"
        )));
    }
    Ok(trimmed.to_owned())
}

/// GET /messages/{peer_id} — authenticated; returns the stored conversation
/// between the current user and the given friend, oldest first. Messages the
/// user deleted for themselves are omitted; messages deleted for everyone are
/// returned as tombstones.
pub async fn history(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(peer_id): Path<Uuid>,
) -> Result<Json<Vec<MessageView>>, AppError> {
    if !state.friends.are_friends(user.id, peer_id).await {
        return Err(AppError::Forbidden);
    }
    Ok(Json(state.messages.history(user.id, peer_id).await))
}

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    scope: DeleteScope,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteScope {
    #[default]
    Me,
    Everyone,
}

#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    status: &'static str,
}

/// DELETE /messages/{id}?scope=me|everyone — authenticated.
/// `scope=me` hides the message for the current user only (any participant).
/// `scope=everyone` tombstones the message for both sides (sender only),
/// purges any image attachment, and notifies connected participants over
/// WebSocket.
pub async fn delete(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(message_id): Path<Uuid>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<DeleteResponse>, AppError> {
    match query.scope {
        DeleteScope::Me => {
            state.messages.delete_for_me(message_id, user.id).await?;
            Ok(Json(DeleteResponse {
                status: "deleted_for_me",
            }))
        }
        DeleteScope::Everyone => {
            let (tombstone, purged_attachment) = state
                .messages
                .delete_for_everyone(message_id, user.id)
                .await?;
            if let Some(attachment_id) = purged_attachment {
                state.attachments.remove(attachment_id).await;
            }
            let event = OutboundEvent::MessageDeleted {
                message_id: tombstone.id,
                sender_id: tombstone.sender_id,
                recipient_id: tombstone.recipient_id,
            };
            state
                .user_hub
                .send_to(tombstone.sender_id, event.clone())
                .await;
            state.user_hub.send_to(tombstone.recipient_id, event).await;
            Ok(Json(DeleteResponse {
                status: "deleted_for_everyone",
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InMemoryMessageStore, MessageKind, validate_body};
    use uuid::Uuid;

    #[test]
    fn validates_message_bodies() {
        assert_eq!(validate_body("  hello  ").unwrap(), "hello");
        assert!(validate_body("").is_err());
        assert!(validate_body("   ").is_err());
        assert!(validate_body(&"a".repeat(4097)).is_err());
    }

    #[tokio::test]
    async fn stores_and_returns_history_for_both_participants() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        store.append(alice, bob, "hi bob".to_owned()).await.unwrap();
        store
            .append(bob, alice, "hi alice".to_owned())
            .await
            .unwrap();

        let alice_history = store.history(alice, bob).await;
        let bob_history = store.history(bob, alice).await;
        assert_eq!(alice_history.len(), 2);
        assert_eq!(bob_history.len(), 2);
        assert_eq!(alice_history[0].body, "hi bob");
        assert_eq!(alice_history[1].body, "hi alice");
    }

    #[tokio::test]
    async fn delete_for_me_hides_message_only_for_that_user() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let message = store.append(alice, bob, "secret".to_owned()).await.unwrap();
        store.delete_for_me(message.id, bob).await.unwrap();

        assert!(store.history(bob, alice).await.is_empty());
        let alice_history = store.history(alice, bob).await;
        assert_eq!(alice_history.len(), 1);
        assert_eq!(alice_history[0].body, "secret");
    }

    #[tokio::test]
    async fn delete_for_everyone_tombstones_for_both_users() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let message = store.append(alice, bob, "oops".to_owned()).await.unwrap();
        let (tombstone, purged_attachment) =
            store.delete_for_everyone(message.id, alice).await.unwrap();
        assert!(tombstone.deleted);
        assert!(purged_attachment.is_none());

        for viewer in [alice, bob] {
            let history = store
                .history(viewer, if viewer == alice { bob } else { alice })
                .await;
            assert_eq!(history.len(), 1);
            assert!(history[0].deleted);
            assert!(history[0].body.is_empty());
        }
    }

    #[tokio::test]
    async fn image_messages_are_stored_and_purged_on_delete_for_everyone() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let attachment_id = Uuid::new_v4();

        let message = store.append_image(alice, bob, attachment_id).await.unwrap();
        assert_eq!(message.kind, MessageKind::Image);
        assert_eq!(message.attachment_id, Some(attachment_id));

        let history = store.history(bob, alice).await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attachment_id, Some(attachment_id));

        let (tombstone, purged_attachment) =
            store.delete_for_everyone(message.id, alice).await.unwrap();
        assert!(tombstone.deleted);
        assert!(tombstone.attachment_id.is_none());
        assert_eq!(purged_attachment, Some(attachment_id));

        let history = store.history(bob, alice).await;
        assert!(history[0].deleted);
        assert!(history[0].attachment_id.is_none());
    }

    #[tokio::test]
    async fn only_sender_can_delete_for_everyone() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let mallory = Uuid::new_v4();

        let message = store.append(alice, bob, "mine".to_owned()).await.unwrap();
        assert!(store.delete_for_everyone(message.id, bob).await.is_err());
        assert!(
            store
                .delete_for_everyone(message.id, mallory)
                .await
                .is_err()
        );
        assert!(store.delete_for_me(message.id, mallory).await.is_err());

        let history = store.history(bob, alice).await;
        assert_eq!(history.len(), 1);
        assert!(!history[0].deleted);
    }
}
