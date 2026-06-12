use crate::{auth, error::AppError, state::AppState, ws::OutboundEvent};
use async_trait::async_trait;
use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;
use uuid::Uuid;

pub const MAX_MESSAGE_BODY_BYTES: usize = 4096;
pub const REPLY_PREVIEW_MAX_CHARS: usize = 140;
pub const ALLOWED_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🙏"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Text,
    Image,
}

impl MessageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageKind::Text => "text",
            MessageKind::Image => "image",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "text" => Ok(MessageKind::Text),
            "image" => Ok(MessageKind::Image),
            _ => Err(AppError::Internal),
        }
    }
}

/// Quoted context embedded in a reply. The body is a truncated snapshot built
/// at read time, so it reflects later delete-for-everyone tombstoning.
#[derive(Clone, Debug, Serialize)]
pub struct ReplyPreview {
    pub id: Uuid,
    pub sender_id: Uuid,
    pub kind: MessageKind,
    pub body: String,
    pub deleted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReactionView {
    pub emoji: String,
    pub user_ids: Vec<Uuid>,
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
    pub reply_to: Option<ReplyPreview>,
    pub reactions: Vec<ReactionView>,
    pub sent_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
    pub deleted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationSummary {
    pub last_message: Option<MessageView>,
    pub unread_count: usize,
}

#[derive(Clone, Debug)]
pub struct ReactionToggle {
    pub added: bool,
    pub sender_id: Uuid,
    pub recipient_id: Uuid,
}

#[async_trait]
pub trait MessageStore: Send + Sync {
    async fn append_text(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        body: String,
        reply_to: Option<Uuid>,
    ) -> Result<MessageView, AppError>;

    /// The attachment must already be validated and bound to this sender and
    /// recipient via the attachment store before calling this.
    async fn append_image(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        attachment_id: Uuid,
        reply_to: Option<Uuid>,
    ) -> Result<MessageView, AppError>;

    async fn history(&self, user_id: Uuid, peer_id: Uuid) -> Result<Vec<MessageView>, AppError>;

    /// Hides the message from `user_id` only. Any conversation participant may
    /// delete any message for themselves.
    async fn delete_for_me(&self, message_id: Uuid, user_id: Uuid) -> Result<(), AppError>;

    /// Replaces the message with a tombstone for both participants. Only the
    /// original sender may delete a message for everyone. Returns the
    /// tombstone view plus the id of the attachment to purge, if any.
    async fn delete_for_everyone(
        &self,
        message_id: Uuid,
        user_id: Uuid,
    ) -> Result<(MessageView, Option<Uuid>), AppError>;

    /// Marks the given messages as delivered to their recipient. Only acks
    /// messages addressed to `recipient_id` that are not yet delivered.
    /// Returns `(message_id, sender_id)` for each message actually updated.
    async fn mark_delivered(
        &self,
        message_ids: &[Uuid],
        recipient_id: Uuid,
    ) -> Result<Vec<(Uuid, Uuid)>, AppError>;

    /// Marks every unread message from `peer_id` to `user_id` as read (and
    /// delivered). Returns the ids of the messages updated.
    async fn mark_read(&self, user_id: Uuid, peer_id: Uuid) -> Result<Vec<Uuid>, AppError>;

    /// Toggles `emoji` by `user_id` on a message in a conversation the user
    /// participates in. Rejects tombstoned messages and unknown emojis.
    async fn toggle_reaction(
        &self,
        message_id: Uuid,
        user_id: Uuid,
        emoji: &str,
    ) -> Result<ReactionToggle, AppError>;

    /// Latest visible message plus unread count for the user's sidebar.
    async fn summary(&self, user_id: Uuid, peer_id: Uuid) -> Result<ConversationSummary, AppError>;
}

pub fn validate_body(body: &str) -> Result<String, AppError> {
    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_MESSAGE_BODY_BYTES {
        return Err(AppError::BadRequest(format!(
            "message body must be 1-{MAX_MESSAGE_BODY_BYTES} bytes"
        )));
    }
    Ok(trimmed.to_owned())
}

pub fn validate_reaction(emoji: &str) -> Result<(), AppError> {
    if ALLOWED_REACTIONS.contains(&emoji) {
        Ok(())
    } else {
        Err(AppError::BadRequest("unsupported reaction".to_owned()))
    }
}

pub fn truncate_preview(body: &str) -> String {
    if body.chars().count() <= REPLY_PREVIEW_MAX_CHARS {
        body.to_owned()
    } else {
        let mut preview = body
            .chars()
            .take(REPLY_PREVIEW_MAX_CHARS)
            .collect::<String>();
        preview.push('…');
        preview
    }
}

// ---------------------------------------------------------------------------
// In-memory implementation (development default)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct StoredMessage {
    id: Uuid,
    sender_id: Uuid,
    recipient_id: Uuid,
    kind: MessageKind,
    body: String,
    attachment_id: Option<Uuid>,
    reply_to: Option<Uuid>,
    sent_at: DateTime<Utc>,
    delivered_at: Option<DateTime<Utc>>,
    read_at: Option<DateTime<Utc>>,
    deleted_for_everyone: bool,
    deleted_for: HashSet<Uuid>,
    reactions: BTreeMap<String, BTreeSet<Uuid>>,
}

impl StoredMessage {
    fn view(&self, conversation: &[StoredMessage]) -> MessageView {
        let reply_to = self.reply_to.and_then(|reply_id| {
            conversation
                .iter()
                .find(|message| message.id == reply_id)
                .map(reply_preview)
        });
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
            reply_to,
            reactions: self
                .reactions
                .iter()
                .filter(|(_, users)| !users.is_empty())
                .map(|(emoji, users)| ReactionView {
                    emoji: emoji.clone(),
                    user_ids: users.iter().copied().collect(),
                })
                .collect(),
            sent_at: self.sent_at,
            delivered_at: self.delivered_at,
            read_at: self.read_at,
            deleted: self.deleted_for_everyone,
        }
    }
}

fn reply_preview(message: &StoredMessage) -> ReplyPreview {
    ReplyPreview {
        id: message.id,
        sender_id: message.sender_id,
        kind: message.kind,
        body: if message.deleted_for_everyone {
            String::new()
        } else {
            truncate_preview(&message.body)
        },
        deleted: message.deleted_for_everyone,
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
    async fn push(&self, message: StoredMessage) -> Result<MessageView, AppError> {
        let key = conversation_key(message.sender_id, message.recipient_id);
        let mut conversations = self.conversations.write().await;
        let conversation = conversations.entry(key).or_default();
        if let Some(reply_id) = message.reply_to
            && !conversation.iter().any(|other| other.id == reply_id)
        {
            return Err(AppError::BadRequest(
                "replied message is not in this conversation".to_owned(),
            ));
        }
        self.message_index.write().await.insert(message.id, key);
        conversation.push(message);
        let conversation = &conversations[&key];
        conversation
            .last()
            .map(|stored| stored.view(conversation))
            .ok_or(AppError::Internal)
    }

    async fn lookup_key(&self, message_id: Uuid) -> Result<ConversationKey, AppError> {
        self.message_index
            .read()
            .await
            .get(&message_id)
            .copied()
            .ok_or(AppError::NotFound)
    }
}

fn new_stored(
    sender_id: Uuid,
    recipient_id: Uuid,
    kind: MessageKind,
    body: String,
    attachment_id: Option<Uuid>,
    reply_to: Option<Uuid>,
) -> StoredMessage {
    StoredMessage {
        id: Uuid::new_v4(),
        sender_id,
        recipient_id,
        kind,
        body,
        attachment_id,
        reply_to,
        sent_at: Utc::now(),
        delivered_at: None,
        read_at: None,
        deleted_for_everyone: false,
        deleted_for: HashSet::new(),
        reactions: BTreeMap::new(),
    }
}

#[async_trait]
impl MessageStore for InMemoryMessageStore {
    async fn append_text(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        body: String,
        reply_to: Option<Uuid>,
    ) -> Result<MessageView, AppError> {
        let body = validate_body(&body)?;
        self.push(new_stored(
            sender_id,
            recipient_id,
            MessageKind::Text,
            body,
            None,
            reply_to,
        ))
        .await
    }

    async fn append_image(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        attachment_id: Uuid,
        reply_to: Option<Uuid>,
    ) -> Result<MessageView, AppError> {
        self.push(new_stored(
            sender_id,
            recipient_id,
            MessageKind::Image,
            String::new(),
            Some(attachment_id),
            reply_to,
        ))
        .await
    }

    async fn history(&self, user_id: Uuid, peer_id: Uuid) -> Result<Vec<MessageView>, AppError> {
        let key = conversation_key(user_id, peer_id);
        Ok(self
            .conversations
            .read()
            .await
            .get(&key)
            .map(|messages| {
                messages
                    .iter()
                    .filter(|message| !message.deleted_for.contains(&user_id))
                    .map(|message| message.view(messages))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn delete_for_me(&self, message_id: Uuid, user_id: Uuid) -> Result<(), AppError> {
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

    async fn delete_for_everyone(
        &self,
        message_id: Uuid,
        user_id: Uuid,
    ) -> Result<(MessageView, Option<Uuid>), AppError> {
        let key = self.lookup_key(message_id).await?;
        if key.0 != user_id && key.1 != user_id {
            return Err(AppError::NotFound);
        }
        let mut conversations = self.conversations.write().await;
        let conversation = conversations.get_mut(&key).ok_or(AppError::NotFound)?;
        let message = conversation
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(AppError::NotFound)?;
        if message.sender_id != user_id {
            return Err(AppError::Forbidden);
        }
        message.deleted_for_everyone = true;
        message.body.clear();
        message.reactions.clear();
        let purged_attachment = message.attachment_id.take();
        let view = conversation
            .iter()
            .find(|message| message.id == message_id)
            .map(|message| message.view(conversation))
            .ok_or(AppError::Internal)?;
        Ok((view, purged_attachment))
    }

    async fn mark_delivered(
        &self,
        message_ids: &[Uuid],
        recipient_id: Uuid,
    ) -> Result<Vec<(Uuid, Uuid)>, AppError> {
        let mut updated = Vec::new();
        let now = Utc::now();
        let index = self.message_index.read().await;
        let mut conversations = self.conversations.write().await;
        for message_id in message_ids {
            let Some(key) = index.get(message_id) else {
                continue;
            };
            let Some(message) = conversations.get_mut(key).and_then(|messages| {
                messages
                    .iter_mut()
                    .find(|message| message.id == *message_id)
            }) else {
                continue;
            };
            if message.recipient_id == recipient_id && message.delivered_at.is_none() {
                message.delivered_at = Some(now);
                updated.push((message.id, message.sender_id));
            }
        }
        Ok(updated)
    }

    async fn mark_read(&self, user_id: Uuid, peer_id: Uuid) -> Result<Vec<Uuid>, AppError> {
        let key = conversation_key(user_id, peer_id);
        let now = Utc::now();
        let mut updated = Vec::new();
        let mut conversations = self.conversations.write().await;
        if let Some(messages) = conversations.get_mut(&key) {
            for message in messages.iter_mut() {
                if message.sender_id == peer_id
                    && message.recipient_id == user_id
                    && message.read_at.is_none()
                {
                    message.read_at = Some(now);
                    message.delivered_at.get_or_insert(now);
                    updated.push(message.id);
                }
            }
        }
        Ok(updated)
    }

    async fn toggle_reaction(
        &self,
        message_id: Uuid,
        user_id: Uuid,
        emoji: &str,
    ) -> Result<ReactionToggle, AppError> {
        validate_reaction(emoji)?;
        let key = self.lookup_key(message_id).await?;
        if key.0 != user_id && key.1 != user_id {
            return Err(AppError::NotFound);
        }
        let mut conversations = self.conversations.write().await;
        let message = conversations
            .get_mut(&key)
            .and_then(|messages| messages.iter_mut().find(|message| message.id == message_id))
            .ok_or(AppError::NotFound)?;
        if message.deleted_for_everyone {
            return Err(AppError::BadRequest(
                "cannot react to a deleted message".to_owned(),
            ));
        }
        let users = message.reactions.entry(emoji.to_owned()).or_default();
        let added = if users.contains(&user_id) {
            users.remove(&user_id);
            false
        } else {
            users.insert(user_id);
            true
        };
        Ok(ReactionToggle {
            added,
            sender_id: message.sender_id,
            recipient_id: message.recipient_id,
        })
    }

    async fn summary(&self, user_id: Uuid, peer_id: Uuid) -> Result<ConversationSummary, AppError> {
        let key = conversation_key(user_id, peer_id);
        let conversations = self.conversations.read().await;
        let Some(messages) = conversations.get(&key) else {
            return Ok(ConversationSummary {
                last_message: None,
                unread_count: 0,
            });
        };
        let last_message = messages
            .iter()
            .rev()
            .find(|message| !message.deleted_for.contains(&user_id))
            .map(|message| message.view(messages));
        let unread_count = messages
            .iter()
            .filter(|message| {
                message.sender_id == peer_id
                    && message.recipient_id == user_id
                    && message.read_at.is_none()
                    && !message.deleted_for_everyone
                    && !message.deleted_for.contains(&user_id)
            })
            .count();
        Ok(ConversationSummary {
            last_message,
            unread_count,
        })
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

/// GET /messages/{peer_id} — authenticated; returns the stored conversation
/// between the current user and the given friend, oldest first. Messages the
/// user deleted for themselves are omitted; messages deleted for everyone are
/// returned as tombstones.
pub async fn history(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(peer_id): Path<Uuid>,
) -> Result<Json<Vec<MessageView>>, AppError> {
    if !state.friends.are_friends(user.id, peer_id).await? {
        return Err(AppError::Forbidden);
    }
    Ok(Json(state.messages.history(user.id, peer_id).await?))
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
                state.attachments.remove(attachment_id).await?;
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
    use super::{
        InMemoryMessageStore, MessageKind, MessageStore, truncate_preview, validate_body,
        validate_reaction,
    };
    use uuid::Uuid;

    #[test]
    fn validates_message_bodies() {
        assert_eq!(validate_body("  hello  ").unwrap(), "hello");
        assert!(validate_body("").is_err());
        assert!(validate_body("   ").is_err());
        assert!(validate_body(&"a".repeat(4097)).is_err());
    }

    #[test]
    fn validates_reactions_and_previews() {
        assert!(validate_reaction("👍").is_ok());
        assert!(validate_reaction("x").is_err());
        assert_eq!(truncate_preview("short"), "short");
        assert_eq!(truncate_preview(&"a".repeat(200)).chars().count(), 141);
    }

    #[tokio::test]
    async fn stores_and_returns_history_for_both_participants() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        store
            .append_text(alice, bob, "hi bob".to_owned(), None)
            .await
            .unwrap();
        store
            .append_text(bob, alice, "hi alice".to_owned(), None)
            .await
            .unwrap();

        let alice_history = store.history(alice, bob).await.unwrap();
        let bob_history = store.history(bob, alice).await.unwrap();
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

        let message = store
            .append_text(alice, bob, "secret".to_owned(), None)
            .await
            .unwrap();
        store.delete_for_me(message.id, bob).await.unwrap();

        assert!(store.history(bob, alice).await.unwrap().is_empty());
        let alice_history = store.history(alice, bob).await.unwrap();
        assert_eq!(alice_history.len(), 1);
        assert_eq!(alice_history[0].body, "secret");
    }

    #[tokio::test]
    async fn delete_for_everyone_tombstones_for_both_users() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let message = store
            .append_text(alice, bob, "oops".to_owned(), None)
            .await
            .unwrap();
        let (tombstone, purged_attachment) =
            store.delete_for_everyone(message.id, alice).await.unwrap();
        assert!(tombstone.deleted);
        assert!(purged_attachment.is_none());

        for viewer in [alice, bob] {
            let history = store
                .history(viewer, if viewer == alice { bob } else { alice })
                .await
                .unwrap();
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

        let message = store
            .append_image(alice, bob, attachment_id, None)
            .await
            .unwrap();
        assert_eq!(message.kind, MessageKind::Image);
        assert_eq!(message.attachment_id, Some(attachment_id));

        let (tombstone, purged_attachment) =
            store.delete_for_everyone(message.id, alice).await.unwrap();
        assert!(tombstone.deleted);
        assert!(tombstone.attachment_id.is_none());
        assert_eq!(purged_attachment, Some(attachment_id));
    }

    #[tokio::test]
    async fn only_sender_can_delete_for_everyone() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let mallory = Uuid::new_v4();

        let message = store
            .append_text(alice, bob, "mine".to_owned(), None)
            .await
            .unwrap();
        assert!(store.delete_for_everyone(message.id, bob).await.is_err());
        assert!(
            store
                .delete_for_everyone(message.id, mallory)
                .await
                .is_err()
        );
        assert!(store.delete_for_me(message.id, mallory).await.is_err());
    }

    #[tokio::test]
    async fn receipts_flow_from_sent_to_delivered_to_read() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let first = store
            .append_text(alice, bob, "one".to_owned(), None)
            .await
            .unwrap();
        let second = store
            .append_text(alice, bob, "two".to_owned(), None)
            .await
            .unwrap();
        assert!(first.delivered_at.is_none() && first.read_at.is_none());

        // Only the true recipient can ack delivery.
        let updated = store.mark_delivered(&[first.id], alice).await.unwrap();
        assert!(updated.is_empty());
        let updated = store.mark_delivered(&[first.id], bob).await.unwrap();
        assert_eq!(updated, vec![(first.id, alice)]);
        // Second ack is a no-op.
        assert!(
            store
                .mark_delivered(&[first.id], bob)
                .await
                .unwrap()
                .is_empty()
        );

        // Reading marks everything from the peer read (and delivered).
        let read_ids = store.mark_read(bob, alice).await.unwrap();
        assert_eq!(read_ids.len(), 2);
        assert!(read_ids.contains(&second.id));
        let history = store.history(alice, bob).await.unwrap();
        assert!(
            history
                .iter()
                .all(|m| m.read_at.is_some() && m.delivered_at.is_some())
        );
        // Re-reading is a no-op.
        assert!(store.mark_read(bob, alice).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reactions_toggle_and_are_visible_in_history() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let mallory = Uuid::new_v4();

        let message = store
            .append_text(alice, bob, "react to me".to_owned(), None)
            .await
            .unwrap();

        let toggle = store.toggle_reaction(message.id, bob, "👍").await.unwrap();
        assert!(toggle.added);
        let toggle = store
            .toggle_reaction(message.id, alice, "👍")
            .await
            .unwrap();
        assert!(toggle.added);

        let history = store.history(alice, bob).await.unwrap();
        assert_eq!(history[0].reactions.len(), 1);
        assert_eq!(history[0].reactions[0].emoji, "👍");
        assert_eq!(history[0].reactions[0].user_ids.len(), 2);

        // Toggle off.
        let toggle = store.toggle_reaction(message.id, bob, "👍").await.unwrap();
        assert!(!toggle.added);
        let history = store.history(alice, bob).await.unwrap();
        assert_eq!(history[0].reactions[0].user_ids.len(), 1);

        // Outsiders cannot react; unknown emojis are rejected.
        assert!(
            store
                .toggle_reaction(message.id, mallory, "👍")
                .await
                .is_err()
        );
        assert!(store.toggle_reaction(message.id, bob, "🤖").await.is_err());

        // Tombstoned messages cannot be reacted to and lose reactions.
        store.delete_for_everyone(message.id, alice).await.unwrap();
        assert!(store.toggle_reaction(message.id, bob, "👍").await.is_err());
        let history = store.history(alice, bob).await.unwrap();
        assert!(history[0].reactions.is_empty());
    }

    #[tokio::test]
    async fn replies_embed_previews_and_track_deletion() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let original = store
            .append_text(alice, bob, "original".to_owned(), None)
            .await
            .unwrap();
        let reply = store
            .append_text(bob, alice, "a reply".to_owned(), Some(original.id))
            .await
            .unwrap();
        let preview = reply.reply_to.expect("reply preview present");
        assert_eq!(preview.id, original.id);
        assert_eq!(preview.body, "original");
        assert!(!preview.deleted);

        // Replying to a message from another conversation is rejected.
        let mallory = Uuid::new_v4();
        let elsewhere = store
            .append_text(alice, mallory, "elsewhere".to_owned(), None)
            .await
            .unwrap();
        assert!(
            store
                .append_text(bob, alice, "bad reply".to_owned(), Some(elsewhere.id))
                .await
                .is_err()
        );

        // Preview reflects later delete-for-everyone.
        store.delete_for_everyone(original.id, alice).await.unwrap();
        let history = store.history(bob, alice).await.unwrap();
        let reply_view = history
            .iter()
            .find(|message| message.id == reply.id)
            .unwrap();
        let preview = reply_view.reply_to.as_ref().unwrap();
        assert!(preview.deleted);
        assert!(preview.body.is_empty());
    }

    #[tokio::test]
    async fn summary_returns_last_message_and_unread_count() {
        let store = InMemoryMessageStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let empty = store.summary(alice, bob).await.unwrap();
        assert!(empty.last_message.is_none());
        assert_eq!(empty.unread_count, 0);

        store
            .append_text(alice, bob, "one".to_owned(), None)
            .await
            .unwrap();
        store
            .append_text(alice, bob, "two".to_owned(), None)
            .await
            .unwrap();

        let bob_summary = store.summary(bob, alice).await.unwrap();
        assert_eq!(bob_summary.unread_count, 2);
        assert_eq!(bob_summary.last_message.unwrap().body, "two");
        let alice_summary = store.summary(alice, bob).await.unwrap();
        assert_eq!(alice_summary.unread_count, 0);

        store.mark_read(bob, alice).await.unwrap();
        let bob_summary = store.summary(bob, alice).await.unwrap();
        assert_eq!(bob_summary.unread_count, 0);
    }
}
