//! Conversations (direct + group), messages, receipts, reactions, pins, and
//! search. A conversation has members; direct conversations have exactly two
//! and are created on demand between friends, group conversations have a
//! title and an owner. Read state is tracked per member via `last_read_at`.

use crate::{auth, error::AppError, state::AppState, users::User, ws::OutboundEvent};
use async_trait::async_trait;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
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
pub const MAX_GROUP_TITLE_CHARS: usize = 60;
pub const MAX_VOICE_DURATION_MS: i32 = 10 * 60 * 1000;

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Direct,
    Group,
}

impl ConversationKind {
    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "direct" => Ok(ConversationKind::Direct),
            "group" => Ok(ConversationKind::Group),
            _ => Err(AppError::Internal),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Text,
    Image,
    Voice,
}

impl MessageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageKind::Text => "text",
            MessageKind::Image => "image",
            MessageKind::Voice => "voice",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "text" => Ok(MessageKind::Text),
            "image" => Ok(MessageKind::Image),
            "voice" => Ok(MessageKind::Voice),
            _ => Err(AppError::Internal),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Owner,
    Admin,
    Member,
}

impl MemberRole {
    pub fn parse(value: &str) -> Self {
        match value {
            "owner" => MemberRole::Owner,
            "admin" => MemberRole::Admin,
            _ => MemberRole::Member,
        }
    }
}

/// A draft message handed to the store after the caller has resolved the
/// conversation and (for media) marked the attachment used.
#[derive(Clone, Debug)]
pub struct NewMessage {
    pub kind: MessageKind,
    pub body: String,
    pub attachment_id: Option<Uuid>,
    pub duration_ms: Option<i32>,
    pub reply_to: Option<Uuid>,
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

/// What a participant sees. `read_by` is the set of *other* members who have
/// read the message (their `last_read_at` is at or after `sent_at`).
#[derive(Clone, Debug, Serialize)]
pub struct MessageView {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sender_id: Uuid,
    pub kind: MessageKind,
    pub body: String,
    pub attachment_id: Option<Uuid>,
    pub duration_ms: Option<i32>,
    pub reply_to: Option<ReplyPreview>,
    pub reactions: Vec<ReactionView>,
    pub pinned: bool,
    pub read_by: Vec<Uuid>,
    pub sent_at: DateTime<Utc>,
    pub deleted: bool,
}

/// Conversation list/detail entry. Member and peer user objects plus presence
/// are filled in by the HTTP handler; the store provides the rest.
#[derive(Clone, Debug, Serialize)]
pub struct ConversationView {
    pub id: Uuid,
    pub kind: ConversationKind,
    pub title: Option<String>,
    pub avatar_attachment_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub member_ids: Vec<Uuid>,
    pub unread_count: usize,
    pub last_message: Option<MessageView>,
}

// ---------------------------------------------------------------------------
// Validation helpers (shared with the Postgres store)
// ---------------------------------------------------------------------------

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

pub fn validate_group_title(title: &str) -> Result<String, AppError> {
    let trimmed = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() || trimmed.chars().count() > MAX_GROUP_TITLE_CHARS {
        return Err(AppError::BadRequest(format!(
            "group title must be 1-{MAX_GROUP_TITLE_CHARS} characters"
        )));
    }
    Ok(trimmed)
}

/// Validates a draft and normalises its fields just before storage.
pub fn prepare_message(mut draft: NewMessage) -> Result<NewMessage, AppError> {
    match draft.kind {
        MessageKind::Text => {
            draft.body = validate_body(&draft.body)?;
            draft.attachment_id = None;
            draft.duration_ms = None;
        }
        MessageKind::Image => {
            if draft.attachment_id.is_none() {
                return Err(AppError::BadRequest(
                    "image requires an attachment".to_owned(),
                ));
            }
            draft.body = String::new();
            draft.duration_ms = None;
        }
        MessageKind::Voice => {
            if draft.attachment_id.is_none() {
                return Err(AppError::BadRequest(
                    "voice requires an attachment".to_owned(),
                ));
            }
            let duration = draft.duration_ms.unwrap_or(0);
            if !(1..=MAX_VOICE_DURATION_MS).contains(&duration) {
                return Err(AppError::BadRequest("invalid voice duration".to_owned()));
            }
            draft.body = String::new();
        }
    }
    Ok(draft)
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
// Store trait
// ---------------------------------------------------------------------------

/// Reaction toggle result: the refreshed message plus whether it was added.
pub struct ReactionResult {
    pub view: MessageView,
    pub added: bool,
    pub emoji: String,
}

#[async_trait]
pub trait ChatStore: Send + Sync {
    /// Returns the direct conversation between `a` and `b`, creating it if it
    /// does not exist. Caller must have verified friendship.
    async fn ensure_direct(&self, a: Uuid, b: Uuid) -> Result<Uuid, AppError>;

    /// Creates a group with `creator` as owner and `members` (already verified
    /// to be friends of the creator) as members.
    async fn create_group(
        &self,
        creator: Uuid,
        title: String,
        members: &[Uuid],
    ) -> Result<Uuid, AppError>;

    async fn conversations_for(&self, user: Uuid) -> Result<Vec<ConversationView>, AppError>;
    async fn conversation_for(&self, id: Uuid, user: Uuid) -> Result<ConversationView, AppError>;
    async fn members(&self, id: Uuid) -> Result<Vec<Uuid>, AppError>;
    async fn is_member(&self, id: Uuid, user: Uuid) -> Result<bool, AppError>;
    async fn add_member(&self, id: Uuid, new_user: Uuid) -> Result<(), AppError>;
    /// Removes `target`. `actor` must be `target` (leave) or the owner.
    async fn remove_member(&self, id: Uuid, actor: Uuid, target: Uuid) -> Result<(), AppError>;

    async fn append(
        &self,
        conversation_id: Uuid,
        sender: Uuid,
        draft: NewMessage,
    ) -> Result<MessageView, AppError>;

    async fn history(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<Vec<MessageView>, AppError>;
    async fn search(
        &self,
        conversation_id: Uuid,
        user: Uuid,
        query: &str,
    ) -> Result<Vec<MessageView>, AppError>;
    async fn pinned(&self, conversation_id: Uuid, user: Uuid)
    -> Result<Vec<MessageView>, AppError>;

    /// Advances `user`'s read cursor to now. Returns the timestamp set.
    async fn mark_read(&self, conversation_id: Uuid, user: Uuid)
    -> Result<DateTime<Utc>, AppError>;

    async fn delete_for_me(&self, message_id: Uuid, user: Uuid) -> Result<(), AppError>;
    async fn delete_for_everyone(
        &self,
        message_id: Uuid,
        user: Uuid,
    ) -> Result<(MessageView, Option<Uuid>), AppError>;
    async fn toggle_reaction(
        &self,
        message_id: Uuid,
        user: Uuid,
        emoji: &str,
    ) -> Result<ReactionResult, AppError>;
    async fn set_pinned(
        &self,
        message_id: Uuid,
        user: Uuid,
        pinned: bool,
    ) -> Result<MessageView, AppError>;

    /// True if `user` may read the attachment, i.e. it is referenced by a
    /// message in a conversation they belong to.
    async fn attachment_visible(&self, attachment_id: Uuid, user: Uuid) -> Result<bool, AppError>;
}

// ---------------------------------------------------------------------------
// In-memory implementation
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct StoredMember {
    role: MemberRole,
    last_read_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
struct StoredMessage {
    id: Uuid,
    sender_id: Uuid,
    kind: MessageKind,
    body: String,
    attachment_id: Option<Uuid>,
    duration_ms: Option<i32>,
    reply_to: Option<Uuid>,
    pinned: bool,
    sent_at: DateTime<Utc>,
    deleted_for_everyone: bool,
    deleted_for: HashSet<Uuid>,
    reactions: BTreeMap<String, BTreeSet<Uuid>>,
}

#[derive(Clone, Debug)]
struct StoredConversation {
    id: Uuid,
    kind: ConversationKind,
    title: Option<String>,
    avatar_attachment_id: Option<Uuid>,
    created_by: Option<Uuid>,
    members: HashMap<Uuid, StoredMember>,
    member_order: Vec<Uuid>,
    messages: Vec<StoredMessage>,
}

impl StoredConversation {
    fn read_by(&self, message: &StoredMessage) -> Vec<Uuid> {
        self.member_order
            .iter()
            .filter(|uid| **uid != message.sender_id)
            .filter(|uid| {
                self.members
                    .get(*uid)
                    .and_then(|member| member.last_read_at)
                    .is_some_and(|read_at| read_at >= message.sent_at)
            })
            .copied()
            .collect()
    }

    fn reply_preview(&self, reply_id: Uuid) -> Option<ReplyPreview> {
        self.messages
            .iter()
            .find(|message| message.id == reply_id)
            .map(|message| ReplyPreview {
                id: message.id,
                sender_id: message.sender_id,
                kind: message.kind,
                body: if message.deleted_for_everyone {
                    String::new()
                } else {
                    truncate_preview(&message.body)
                },
                deleted: message.deleted_for_everyone,
            })
    }

    fn view(&self, message: &StoredMessage) -> MessageView {
        MessageView {
            id: message.id,
            conversation_id: self.id,
            sender_id: message.sender_id,
            kind: message.kind,
            body: if message.deleted_for_everyone {
                String::new()
            } else {
                message.body.clone()
            },
            attachment_id: if message.deleted_for_everyone {
                None
            } else {
                message.attachment_id
            },
            duration_ms: if message.deleted_for_everyone {
                None
            } else {
                message.duration_ms
            },
            reply_to: message.reply_to.and_then(|id| self.reply_preview(id)),
            reactions: message
                .reactions
                .iter()
                .filter(|(_, users)| !users.is_empty())
                .map(|(emoji, users)| ReactionView {
                    emoji: emoji.clone(),
                    user_ids: users.iter().copied().collect(),
                })
                .collect(),
            pinned: message.pinned,
            read_by: self.read_by(message),
            sent_at: message.sent_at,
            deleted: message.deleted_for_everyone,
        }
    }

    fn unread_for(&self, user: Uuid) -> usize {
        let cursor = self
            .members
            .get(&user)
            .and_then(|member| member.last_read_at);
        self.messages
            .iter()
            .filter(|message| {
                message.sender_id != user
                    && !message.deleted_for_everyone
                    && !message.deleted_for.contains(&user)
                    && cursor.is_none_or(|read_at| message.sent_at > read_at)
            })
            .count()
    }

    fn summary_view(&self, user: Uuid) -> ConversationView {
        let last_message = self
            .messages
            .iter()
            .rev()
            .find(|message| !message.deleted_for.contains(&user))
            .map(|message| self.view(message));
        ConversationView {
            id: self.id,
            kind: self.kind,
            title: self.title.clone(),
            avatar_attachment_id: self.avatar_attachment_id,
            created_by: self.created_by,
            member_ids: self.member_order.clone(),
            unread_count: self.unread_for(user),
            last_message,
        }
    }
}

fn direct_key(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a <= b { (a, b) } else { (b, a) }
}

#[derive(Default)]
struct ChatData {
    conversations: HashMap<Uuid, StoredConversation>,
    direct_index: HashMap<(Uuid, Uuid), Uuid>,
    message_index: HashMap<Uuid, Uuid>,
}

impl ChatData {
    fn require_member(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<&StoredConversation, AppError> {
        let conversation = self
            .conversations
            .get(&conversation_id)
            .ok_or(AppError::NotFound)?;
        if !conversation.members.contains_key(&user) {
            return Err(AppError::Forbidden);
        }
        Ok(conversation)
    }

    fn message_conversation(&self, message_id: Uuid) -> Result<Uuid, AppError> {
        self.message_index
            .get(&message_id)
            .copied()
            .ok_or(AppError::NotFound)
    }
}

#[derive(Clone, Default)]
pub struct InMemoryChatStore {
    data: Arc<RwLock<ChatData>>,
}

#[async_trait]
impl ChatStore for InMemoryChatStore {
    async fn ensure_direct(&self, a: Uuid, b: Uuid) -> Result<Uuid, AppError> {
        let key = direct_key(a, b);
        let mut data = self.data.write().await;
        if let Some(id) = data.direct_index.get(&key) {
            return Ok(*id);
        }
        let id = Uuid::new_v4();
        let mut members = HashMap::new();
        members.insert(
            a,
            StoredMember {
                role: MemberRole::Member,
                last_read_at: None,
            },
        );
        members.insert(
            b,
            StoredMember {
                role: MemberRole::Member,
                last_read_at: None,
            },
        );
        data.conversations.insert(
            id,
            StoredConversation {
                id,
                kind: ConversationKind::Direct,
                title: None,
                avatar_attachment_id: None,
                created_by: None,
                members,
                member_order: vec![a, b],
                messages: Vec::new(),
            },
        );
        data.direct_index.insert(key, id);
        Ok(id)
    }

    async fn create_group(
        &self,
        creator: Uuid,
        title: String,
        members: &[Uuid],
    ) -> Result<Uuid, AppError> {
        let title = validate_group_title(&title)?;
        let id = Uuid::new_v4();
        let mut member_map = HashMap::new();
        let mut order = Vec::new();
        member_map.insert(
            creator,
            StoredMember {
                role: MemberRole::Owner,
                last_read_at: None,
            },
        );
        order.push(creator);
        for member in members {
            if *member == creator || member_map.contains_key(member) {
                continue;
            }
            member_map.insert(
                *member,
                StoredMember {
                    role: MemberRole::Member,
                    last_read_at: None,
                },
            );
            order.push(*member);
        }
        let mut data = self.data.write().await;
        data.conversations.insert(
            id,
            StoredConversation {
                id,
                kind: ConversationKind::Group,
                title: Some(title),
                avatar_attachment_id: None,
                created_by: Some(creator),
                members: member_map,
                member_order: order,
                messages: Vec::new(),
            },
        );
        Ok(id)
    }

    async fn conversations_for(&self, user: Uuid) -> Result<Vec<ConversationView>, AppError> {
        let data = self.data.read().await;
        let mut views = data
            .conversations
            .values()
            .filter(|conversation| conversation.members.contains_key(&user))
            .map(|conversation| conversation.summary_view(user))
            .collect::<Vec<_>>();
        views.sort_by(|a, b| {
            let a_at = a.last_message.as_ref().map(|message| message.sent_at);
            let b_at = b.last_message.as_ref().map(|message| message.sent_at);
            b_at.cmp(&a_at)
        });
        Ok(views)
    }

    async fn conversation_for(&self, id: Uuid, user: Uuid) -> Result<ConversationView, AppError> {
        let data = self.data.read().await;
        Ok(data.require_member(id, user)?.summary_view(user))
    }

    async fn members(&self, id: Uuid) -> Result<Vec<Uuid>, AppError> {
        let data = self.data.read().await;
        Ok(data
            .conversations
            .get(&id)
            .ok_or(AppError::NotFound)?
            .member_order
            .clone())
    }

    async fn is_member(&self, id: Uuid, user: Uuid) -> Result<bool, AppError> {
        let data = self.data.read().await;
        Ok(data
            .conversations
            .get(&id)
            .is_some_and(|conversation| conversation.members.contains_key(&user)))
    }

    async fn add_member(&self, id: Uuid, new_user: Uuid) -> Result<(), AppError> {
        let mut data = self.data.write().await;
        let conversation = data.conversations.get_mut(&id).ok_or(AppError::NotFound)?;
        if conversation.kind != ConversationKind::Group {
            return Err(AppError::BadRequest("not a group conversation".to_owned()));
        }
        if conversation.members.contains_key(&new_user) {
            return Err(AppError::Conflict);
        }
        conversation.members.insert(
            new_user,
            StoredMember {
                role: MemberRole::Member,
                last_read_at: None,
            },
        );
        conversation.member_order.push(new_user);
        Ok(())
    }

    async fn remove_member(&self, id: Uuid, actor: Uuid, target: Uuid) -> Result<(), AppError> {
        let mut data = self.data.write().await;
        let conversation = data.conversations.get_mut(&id).ok_or(AppError::NotFound)?;
        if conversation.kind != ConversationKind::Group {
            return Err(AppError::BadRequest("not a group conversation".to_owned()));
        }
        let actor_role = conversation.members.get(&actor).map(|member| member.role);
        let is_owner = actor_role == Some(MemberRole::Owner);
        if actor != target && !is_owner {
            return Err(AppError::Forbidden);
        }
        if conversation.members.remove(&target).is_none() {
            return Err(AppError::NotFound);
        }
        conversation.member_order.retain(|uid| *uid != target);
        Ok(())
    }

    async fn append(
        &self,
        conversation_id: Uuid,
        sender: Uuid,
        draft: NewMessage,
    ) -> Result<MessageView, AppError> {
        let draft = prepare_message(draft)?;
        let mut data = self.data.write().await;
        let now = Utc::now();
        let conversation = data
            .conversations
            .get_mut(&conversation_id)
            .ok_or(AppError::NotFound)?;
        if !conversation.members.contains_key(&sender) {
            return Err(AppError::Forbidden);
        }
        if let Some(reply_id) = draft.reply_to
            && !conversation
                .messages
                .iter()
                .any(|message| message.id == reply_id)
        {
            return Err(AppError::BadRequest(
                "replied message is not in this conversation".to_owned(),
            ));
        }
        let message = StoredMessage {
            id: Uuid::new_v4(),
            sender_id: sender,
            kind: draft.kind,
            body: draft.body,
            attachment_id: draft.attachment_id,
            duration_ms: draft.duration_ms,
            reply_to: draft.reply_to,
            pinned: false,
            sent_at: now,
            deleted_for_everyone: false,
            deleted_for: HashSet::new(),
            reactions: BTreeMap::new(),
        };
        let message_id = message.id;
        if let Some(member) = conversation.members.get_mut(&sender) {
            member.last_read_at = Some(now);
        }
        conversation.messages.push(message);
        data.message_index.insert(message_id, conversation_id);
        let conversation = &data.conversations[&conversation_id];
        let stored = conversation.messages.last().ok_or(AppError::Internal)?;
        Ok(conversation.view(stored))
    }

    async fn history(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<Vec<MessageView>, AppError> {
        let data = self.data.read().await;
        let conversation = data.require_member(conversation_id, user)?;
        Ok(conversation
            .messages
            .iter()
            .filter(|message| !message.deleted_for.contains(&user))
            .map(|message| conversation.view(message))
            .collect())
    }

    async fn search(
        &self,
        conversation_id: Uuid,
        user: Uuid,
        query: &str,
    ) -> Result<Vec<MessageView>, AppError> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let data = self.data.read().await;
        let conversation = data.require_member(conversation_id, user)?;
        Ok(conversation
            .messages
            .iter()
            .filter(|message| {
                !message.deleted_for.contains(&user)
                    && !message.deleted_for_everyone
                    && message.kind == MessageKind::Text
                    && message.body.to_lowercase().contains(&needle)
            })
            .map(|message| conversation.view(message))
            .collect())
    }

    async fn pinned(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<Vec<MessageView>, AppError> {
        let data = self.data.read().await;
        let conversation = data.require_member(conversation_id, user)?;
        Ok(conversation
            .messages
            .iter()
            .filter(|message| {
                message.pinned
                    && !message.deleted_for.contains(&user)
                    && !message.deleted_for_everyone
            })
            .map(|message| conversation.view(message))
            .collect())
    }

    async fn mark_read(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<DateTime<Utc>, AppError> {
        let mut data = self.data.write().await;
        let conversation = data
            .conversations
            .get_mut(&conversation_id)
            .ok_or(AppError::NotFound)?;
        let member = conversation
            .members
            .get_mut(&user)
            .ok_or(AppError::Forbidden)?;
        let now = Utc::now();
        member.last_read_at = Some(now);
        Ok(now)
    }

    async fn delete_for_me(&self, message_id: Uuid, user: Uuid) -> Result<(), AppError> {
        let mut data = self.data.write().await;
        let conversation_id = data.message_conversation(message_id)?;
        let conversation = data
            .conversations
            .get_mut(&conversation_id)
            .ok_or(AppError::NotFound)?;
        if !conversation.members.contains_key(&user) {
            return Err(AppError::Forbidden);
        }
        let message = conversation
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(AppError::NotFound)?;
        message.deleted_for.insert(user);
        Ok(())
    }

    async fn delete_for_everyone(
        &self,
        message_id: Uuid,
        user: Uuid,
    ) -> Result<(MessageView, Option<Uuid>), AppError> {
        let mut data = self.data.write().await;
        let conversation_id = data.message_conversation(message_id)?;
        let conversation = data
            .conversations
            .get_mut(&conversation_id)
            .ok_or(AppError::NotFound)?;
        if !conversation.members.contains_key(&user) {
            return Err(AppError::Forbidden);
        }
        let message = conversation
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(AppError::NotFound)?;
        if message.sender_id != user {
            return Err(AppError::Forbidden);
        }
        message.deleted_for_everyone = true;
        message.body.clear();
        message.pinned = false;
        message.duration_ms = None;
        message.reactions.clear();
        let purged = message.attachment_id.take();
        let conversation = &data.conversations[&conversation_id];
        let stored = conversation
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .ok_or(AppError::Internal)?;
        Ok((conversation.view(stored), purged))
    }

    async fn toggle_reaction(
        &self,
        message_id: Uuid,
        user: Uuid,
        emoji: &str,
    ) -> Result<ReactionResult, AppError> {
        validate_reaction(emoji)?;
        let mut data = self.data.write().await;
        let conversation_id = data.message_conversation(message_id)?;
        let conversation = data
            .conversations
            .get_mut(&conversation_id)
            .ok_or(AppError::NotFound)?;
        if !conversation.members.contains_key(&user) {
            return Err(AppError::Forbidden);
        }
        let message = conversation
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(AppError::NotFound)?;
        if message.deleted_for_everyone {
            return Err(AppError::BadRequest(
                "cannot react to a deleted message".to_owned(),
            ));
        }
        let users = message.reactions.entry(emoji.to_owned()).or_default();
        let added = if users.contains(&user) {
            users.remove(&user);
            false
        } else {
            users.insert(user);
            true
        };
        let conversation = &data.conversations[&conversation_id];
        let stored = conversation
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .ok_or(AppError::Internal)?;
        Ok(ReactionResult {
            view: conversation.view(stored),
            added,
            emoji: emoji.to_owned(),
        })
    }

    async fn set_pinned(
        &self,
        message_id: Uuid,
        user: Uuid,
        pinned: bool,
    ) -> Result<MessageView, AppError> {
        let mut data = self.data.write().await;
        let conversation_id = data.message_conversation(message_id)?;
        let conversation = data
            .conversations
            .get_mut(&conversation_id)
            .ok_or(AppError::NotFound)?;
        if !conversation.members.contains_key(&user) {
            return Err(AppError::Forbidden);
        }
        let message = conversation
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(AppError::NotFound)?;
        if message.deleted_for_everyone {
            return Err(AppError::BadRequest(
                "cannot pin a deleted message".to_owned(),
            ));
        }
        message.pinned = pinned;
        let conversation = &data.conversations[&conversation_id];
        let stored = conversation
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .ok_or(AppError::Internal)?;
        Ok(conversation.view(stored))
    }

    async fn attachment_visible(&self, attachment_id: Uuid, user: Uuid) -> Result<bool, AppError> {
        let data = self.data.read().await;
        Ok(data.conversations.values().any(|conversation| {
            conversation.members.contains_key(&user)
                && conversation
                    .messages
                    .iter()
                    .any(|message| message.attachment_id == Some(attachment_id))
        }))
    }
}

// ---------------------------------------------------------------------------
// HTTP API
// ---------------------------------------------------------------------------

/// Conversation enriched with the user objects and presence the frontend needs.
#[derive(Debug, Serialize)]
pub struct ConversationDto {
    pub id: Uuid,
    pub kind: ConversationKind,
    pub title: Option<String>,
    pub avatar_attachment_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub unread_count: usize,
    pub last_message: Option<MessageView>,
    pub members: Vec<MemberDto>,
    /// For direct conversations, the other participant.
    pub other_user: Option<User>,
    /// For direct conversations, whether the other participant is online.
    pub online: bool,
}

#[derive(Debug, Serialize)]
pub struct MemberDto {
    #[serde(flatten)]
    pub user: User,
    pub online: bool,
}

async fn enrich(state: &AppState, view: ConversationView, me: Uuid) -> ConversationDto {
    let mut members = Vec::with_capacity(view.member_ids.len());
    let mut other_user = None;
    let mut online = false;
    for member_id in &view.member_ids {
        if let Some(user) = state.users.find_by_id(*member_id).await {
            let member_online = state.user_hub.is_online(*member_id).await;
            if view.kind == ConversationKind::Direct && *member_id != me {
                other_user = Some(user.clone());
                online = member_online;
            }
            members.push(MemberDto {
                user,
                online: member_online,
            });
        }
    }
    ConversationDto {
        id: view.id,
        kind: view.kind,
        title: view.title,
        avatar_attachment_id: view.avatar_attachment_id,
        created_by: view.created_by,
        unread_count: view.unread_count,
        last_message: view.last_message,
        members,
        other_user,
        online,
    }
}

/// GET /conversations — the user's conversation list, enriched, newest first.
pub async fn list_conversations(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
) -> Result<Json<Vec<ConversationDto>>, AppError> {
    let views = state.chat.conversations_for(user.id).await?;
    let mut dtos = Vec::with_capacity(views.len());
    for view in views {
        dtos.push(enrich(&state, view, user.id).await);
    }
    Ok(Json(dtos))
}

#[derive(Debug, Deserialize)]
pub struct DirectRequest {
    user_id: Uuid,
}

/// POST /conversations/direct — get or create the direct conversation with a friend.
pub async fn open_direct(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Json(request): Json<DirectRequest>,
) -> Result<Json<ConversationDto>, AppError> {
    if !state.friends.are_friends(user.id, request.user_id).await? {
        return Err(AppError::Forbidden);
    }
    let id = state.chat.ensure_direct(user.id, request.user_id).await?;
    let view = state.chat.conversation_for(id, user.id).await?;
    Ok(Json(enrich(&state, view, user.id).await))
}

#[derive(Debug, Deserialize)]
pub struct GroupRequest {
    title: String,
    member_ids: Vec<Uuid>,
}

/// POST /conversations/group — create a group with the given friends.
pub async fn create_group(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Json(request): Json<GroupRequest>,
) -> Result<(StatusCode, Json<ConversationDto>), AppError> {
    let mut members = Vec::new();
    for member_id in &request.member_ids {
        if *member_id == user.id {
            continue;
        }
        if !state.friends.are_friends(user.id, *member_id).await? {
            return Err(AppError::BadRequest(
                "can only add friends to a group".to_owned(),
            ));
        }
        members.push(*member_id);
    }
    if members.is_empty() {
        return Err(AppError::BadRequest(
            "a group needs at least one friend".to_owned(),
        ));
    }
    let id = state
        .chat
        .create_group(user.id, request.title, &members)
        .await?;
    let view = state.chat.conversation_for(id, user.id).await?;
    let dto = enrich(&state, view, user.id).await;
    let member_ids = state.chat.members(id).await?;
    broadcast_to(
        &state,
        &member_ids,
        OutboundEvent::ConversationUpdated {
            conversation_id: id,
        },
    )
    .await;
    Ok((StatusCode::CREATED, Json(dto)))
}

#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    user_id: Uuid,
}

/// POST /conversations/{id}/members — add a friend of the caller to a group.
pub async fn add_member(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(conversation_id): Path<Uuid>,
    Json(request): Json<AddMemberRequest>,
) -> Result<Json<ConversationDto>, AppError> {
    if !state.chat.is_member(conversation_id, user.id).await? {
        return Err(AppError::Forbidden);
    }
    if !state.friends.are_friends(user.id, request.user_id).await? {
        return Err(AppError::BadRequest("can only add your friends".to_owned()));
    }
    state
        .chat
        .add_member(conversation_id, request.user_id)
        .await?;
    let member_ids = state.chat.members(conversation_id).await?;
    broadcast_to(
        &state,
        &member_ids,
        OutboundEvent::ConversationUpdated { conversation_id },
    )
    .await;
    let view = state
        .chat
        .conversation_for(conversation_id, user.id)
        .await?;
    Ok(Json(enrich(&state, view, user.id).await))
}

/// DELETE /conversations/{id}/members/{user_id} — leave or (owner) remove.
pub async fn remove_member(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path((conversation_id, target)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    let member_ids_before = state.chat.members(conversation_id).await?;
    state
        .chat
        .remove_member(conversation_id, user.id, target)
        .await?;
    broadcast_to(
        &state,
        &member_ids_before,
        OutboundEvent::ConversationUpdated { conversation_id },
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// GET /conversations/{id}/messages — full history oldest first.
pub async fn history(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(conversation_id): Path<Uuid>,
) -> Result<Json<Vec<MessageView>>, AppError> {
    Ok(Json(state.chat.history(conversation_id, user.id).await?))
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    q: String,
}

/// GET /conversations/{id}/search?q= — text messages matching q.
pub async fn search(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(conversation_id): Path<Uuid>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<MessageView>>, AppError> {
    Ok(Json(
        state
            .chat
            .search(conversation_id, user.id, &query.q)
            .await?,
    ))
}

/// GET /conversations/{id}/pins — pinned messages.
pub async fn pinned(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(conversation_id): Path<Uuid>,
) -> Result<Json<Vec<MessageView>>, AppError> {
    Ok(Json(state.chat.pinned(conversation_id, user.id).await?))
}

#[derive(Debug, Deserialize)]
pub struct PinRequest {
    pinned: bool,
}

/// POST /messages/{id}/pin — pin or unpin a message (any member).
pub async fn set_pin(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(message_id): Path<Uuid>,
    Json(request): Json<PinRequest>,
) -> Result<Json<MessageView>, AppError> {
    let view = state
        .chat
        .set_pinned(message_id, user.id, request.pinned)
        .await?;
    let member_ids = state.chat.members(view.conversation_id).await?;
    broadcast_to(
        &state,
        &member_ids,
        OutboundEvent::MessagePinned {
            conversation_id: view.conversation_id,
            message_id,
            pinned: request.pinned,
        },
    )
    .await;
    Ok(Json(view))
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

/// DELETE /messages/{id}?scope=me|everyone.
pub async fn delete(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(message_id): Path<Uuid>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<DeleteResponse>, AppError> {
    match query.scope {
        DeleteScope::Me => {
            state.chat.delete_for_me(message_id, user.id).await?;
            Ok(Json(DeleteResponse {
                status: "deleted_for_me",
            }))
        }
        DeleteScope::Everyone => {
            let (tombstone, purged) = state.chat.delete_for_everyone(message_id, user.id).await?;
            if let Some(attachment_id) = purged {
                state.attachments.remove(attachment_id).await?;
            }
            let member_ids = state.chat.members(tombstone.conversation_id).await?;
            broadcast_to(
                &state,
                &member_ids,
                OutboundEvent::MessageDeleted {
                    conversation_id: tombstone.conversation_id,
                    message_id: tombstone.id,
                },
            )
            .await;
            Ok(Json(DeleteResponse {
                status: "deleted_for_everyone",
            }))
        }
    }
}

pub async fn broadcast_to(state: &AppState, member_ids: &[Uuid], event: OutboundEvent) {
    for member_id in member_ids {
        state.user_hub.send_to(*member_id, event.clone()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(body: &str) -> NewMessage {
        NewMessage {
            kind: MessageKind::Text,
            body: body.to_owned(),
            attachment_id: None,
            duration_ms: None,
            reply_to: None,
        }
    }

    #[tokio::test]
    async fn direct_is_created_once_and_shared() {
        let store = InMemoryChatStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let first = store.ensure_direct(alice, bob).await.unwrap();
        let second = store.ensure_direct(bob, alice).await.unwrap();
        assert_eq!(first, second);
        assert!(store.is_member(first, alice).await.unwrap());
        assert!(store.is_member(first, bob).await.unwrap());
    }

    #[tokio::test]
    async fn messages_history_and_unread_track_read_cursor() {
        let store = InMemoryChatStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let convo = store.ensure_direct(alice, bob).await.unwrap();

        store.append(convo, alice, text("hi")).await.unwrap();
        store.append(convo, alice, text("there")).await.unwrap();

        assert_eq!(
            store
                .conversation_for(convo, alice)
                .await
                .unwrap()
                .unread_count,
            0
        );
        assert_eq!(
            store
                .conversation_for(convo, bob)
                .await
                .unwrap()
                .unread_count,
            2
        );

        store.mark_read(convo, bob).await.unwrap();
        assert_eq!(
            store
                .conversation_for(convo, bob)
                .await
                .unwrap()
                .unread_count,
            0
        );

        let history = store.history(convo, alice).await.unwrap();
        assert!(history.iter().all(|m| m.read_by == vec![bob]));
    }

    #[tokio::test]
    async fn non_member_cannot_read_or_post() {
        let store = InMemoryChatStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let mallory = Uuid::new_v4();
        let convo = store.ensure_direct(alice, bob).await.unwrap();

        assert!(store.history(convo, mallory).await.is_err());
        assert!(store.append(convo, mallory, text("intrude")).await.is_err());
    }

    #[tokio::test]
    async fn group_membership_and_messaging() {
        let store = InMemoryChatStore::default();
        let owner = Uuid::new_v4();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let convo = store
            .create_group(owner, "Team".to_owned(), &[a, b])
            .await
            .unwrap();
        assert_eq!(store.members(convo).await.unwrap().len(), 3);

        store.append(convo, owner, text("welcome")).await.unwrap();
        assert_eq!(
            store.conversation_for(convo, a).await.unwrap().unread_count,
            1
        );
        assert_eq!(
            store.conversation_for(convo, b).await.unwrap().unread_count,
            1
        );

        store.mark_read(convo, a).await.unwrap();
        let history = store.history(convo, owner).await.unwrap();
        assert_eq!(history[0].read_by, vec![a]);
        store.mark_read(convo, b).await.unwrap();
        let history = store.history(convo, owner).await.unwrap();
        assert_eq!(history[0].read_by.len(), 2);

        let c = Uuid::new_v4();
        assert!(store.remove_member(convo, a, b).await.is_err());
        store.remove_member(convo, owner, b).await.unwrap();
        store.add_member(convo, c).await.unwrap();
        store.remove_member(convo, c, c).await.unwrap();
        assert_eq!(store.members(convo).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn pin_search_and_delete_semantics() {
        let store = InMemoryChatStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let convo = store.ensure_direct(alice, bob).await.unwrap();

        let m1 = store
            .append(convo, alice, text("buy milk and eggs"))
            .await
            .unwrap();
        store.append(convo, bob, text("ok")).await.unwrap();

        let hits = store.search(convo, bob, "MILK").await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, m1.id);

        store.set_pinned(m1.id, bob, true).await.unwrap();
        assert_eq!(store.pinned(convo, alice).await.unwrap().len(), 1);
        store.set_pinned(m1.id, alice, false).await.unwrap();
        assert!(store.pinned(convo, alice).await.unwrap().is_empty());

        store.delete_for_me(m1.id, bob).await.unwrap();
        assert!(
            store
                .history(convo, bob)
                .await
                .unwrap()
                .iter()
                .all(|m| m.id != m1.id)
        );
        assert!(
            store
                .history(convo, alice)
                .await
                .unwrap()
                .iter()
                .any(|m| m.id == m1.id)
        );

        assert!(store.delete_for_everyone(m1.id, bob).await.is_err());
        let (tombstone, _) = store.delete_for_everyone(m1.id, alice).await.unwrap();
        assert!(tombstone.deleted);
        assert!(store.search(convo, alice, "milk").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reactions_and_replies_in_groups() {
        let store = InMemoryChatStore::default();
        let owner = Uuid::new_v4();
        let a = Uuid::new_v4();
        let convo = store
            .create_group(owner, "G".to_owned(), &[a])
            .await
            .unwrap();
        let original = store.append(convo, owner, text("original")).await.unwrap();

        let reply_draft = NewMessage {
            reply_to: Some(original.id),
            ..text("reply")
        };
        let reply = store.append(convo, a, reply_draft).await.unwrap();
        assert_eq!(reply.reply_to.as_ref().unwrap().body, "original");

        let result = store.toggle_reaction(original.id, a, "👍").await.unwrap();
        assert!(result.added);
        assert_eq!(result.view.reactions[0].user_ids, vec![a]);
        assert!(
            store
                .toggle_reaction(original.id, Uuid::new_v4(), "👍")
                .await
                .is_err()
        );
        assert!(store.toggle_reaction(original.id, a, "🤖").await.is_err());
    }

    #[tokio::test]
    async fn voice_requires_valid_duration() {
        let store = InMemoryChatStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let convo = store.ensure_direct(alice, bob).await.unwrap();

        let bad = NewMessage {
            kind: MessageKind::Voice,
            body: String::new(),
            attachment_id: Some(Uuid::new_v4()),
            duration_ms: Some(0),
            reply_to: None,
        };
        assert!(store.append(convo, alice, bad).await.is_err());

        let good = NewMessage {
            kind: MessageKind::Voice,
            body: String::new(),
            attachment_id: Some(Uuid::new_v4()),
            duration_ms: Some(3500),
            reply_to: None,
        };
        let view = store.append(convo, alice, good).await.unwrap();
        assert_eq!(view.kind, MessageKind::Voice);
        assert_eq!(view.duration_ms, Some(3500));
    }
}
