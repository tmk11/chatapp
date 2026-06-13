//! Postgres implementations of the storage traits, used when DATABASE_URL is
//! configured. Authorization checks (membership, sender-only, owner-only)
//! mirror the in-memory stores exactly.

use crate::{
    attachments::{AttachmentStore, validate_media},
    chat::{
        ChatStore, ConversationKind, ConversationView, MemberRole, MessageKind, MessageView,
        NewMessage, ReactionResult, ReactionView, ReplyPreview, prepare_message, truncate_preview,
        validate_group_title,
    },
    error::AppError,
    friends::{FriendRequest, FriendStore, RequestOutcome, friendship_key},
    users::{StoredUser, User, UserStore, normalize_phone},
};
use async_trait::async_trait;
use axum::body::Bytes;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row, postgres::PgRow};
use std::collections::HashMap;
use tracing::error;
use uuid::Uuid;

fn db_error(error: sqlx::Error) -> AppError {
    error!(%error, "database error");
    AppError::Internal
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PgUserStore {
    pool: PgPool,
}

impl PgUserStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn user_from_row(row: &PgRow) -> User {
    User {
        id: row.get("id"),
        phone: row.get("phone"),
        display_name: row.get("display_name"),
        avatar_attachment_id: row.get("avatar_attachment_id"),
        last_seen_at: row.get("last_seen_at"),
        created_at: row.get("created_at"),
    }
}

const USER_COLUMNS: &str =
    "id, phone, display_name, avatar_attachment_id, last_seen_at, created_at";

#[async_trait]
impl UserStore for PgUserStore {
    async fn create_user(
        &self,
        phone: String,
        display_name: String,
        password_hash: String,
    ) -> Result<User, AppError> {
        let normalized_phone = normalize_phone(&phone)?;
        let row = sqlx::query(
            "INSERT INTO users (id, phone, display_name, password_hash)
             VALUES ($1, $2, $3, $4)
             RETURNING id, phone, display_name, avatar_attachment_id, last_seen_at, created_at",
        )
        .bind(Uuid::new_v4())
        .bind(&normalized_phone)
        .bind(&display_name)
        .bind(&password_hash)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            if is_unique_violation(&error) {
                AppError::Conflict
            } else {
                db_error(error)
            }
        })?;
        Ok(user_from_row(&row))
    }

    async fn find_by_phone(&self, phone: &str) -> Option<StoredUser> {
        let normalized_phone = normalize_phone(phone).ok()?;
        let row = sqlx::query(&format!(
            "SELECT {USER_COLUMNS}, password_hash FROM users WHERE phone = $1"
        ))
        .bind(&normalized_phone)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)
        .ok()??;
        Some(StoredUser {
            user: user_from_row(&row),
            password_hash: row.get("password_hash"),
        })
    }

    async fn find_by_id(&self, id: Uuid) -> Option<User> {
        let row = sqlx::query(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)
            .ok()??;
        Some(user_from_row(&row))
    }

    async fn set_last_seen(&self, id: Uuid, at: DateTime<Utc>) {
        let _ = sqlx::query("UPDATE users SET last_seen_at = $2 WHERE id = $1")
            .bind(id)
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(db_error);
    }

    async fn set_avatar(
        &self,
        id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(Option<Uuid>, User), AppError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let previous: Option<Uuid> =
            sqlx::query("SELECT avatar_attachment_id FROM users WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?
                .ok_or(AppError::NotFound)?
                .get("avatar_attachment_id");
        let row = sqlx::query(&format!(
            "UPDATE users SET avatar_attachment_id = $2 WHERE id = $1 RETURNING {USER_COLUMNS}"
        ))
        .bind(id)
        .bind(attachment_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok((previous, user_from_row(&row)))
    }

    async fn avatar_in_use(&self, attachment_id: Uuid) -> bool {
        sqlx::query("SELECT EXISTS(SELECT 1 FROM users WHERE avatar_attachment_id = $1) AS found")
            .bind(attachment_id)
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get::<bool, _>("found"))
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Friends
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PgFriendStore {
    pool: PgPool,
}

impl PgFriendStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn friend_request_from_row(row: &PgRow) -> FriendRequest {
    FriendRequest {
        id: row.get("id"),
        from_user_id: row.get("from_user_id"),
        to_user_id: row.get("to_user_id"),
        created_at: row.get("created_at"),
    }
}

#[async_trait]
impl FriendStore for PgFriendStore {
    async fn are_friends(&self, a: Uuid, b: Uuid) -> Result<bool, AppError> {
        let (user_a, user_b) = friendship_key(a, b);
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM friendships WHERE user_a = $1 AND user_b = $2) AS found",
        )
        .bind(user_a)
        .bind(user_b)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(row.get("found"))
    }

    async fn send_request(&self, from: Uuid, to: Uuid) -> Result<RequestOutcome, AppError> {
        if from == to {
            return Err(AppError::BadRequest(
                "cannot send a friend request to yourself".to_owned(),
            ));
        }
        if self.are_friends(from, to).await? {
            return Err(AppError::Conflict);
        }

        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let existing = sqlx::query(
            "SELECT id FROM friend_requests WHERE from_user_id = $1 AND to_user_id = $2",
        )
        .bind(from)
        .bind(to)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        if let Some(row) = existing {
            tx.commit().await.map_err(db_error)?;
            return Ok(RequestOutcome::Pending(row.get("id")));
        }

        let mutual = sqlx::query(
            "DELETE FROM friend_requests WHERE from_user_id = $1 AND to_user_id = $2 RETURNING id",
        )
        .bind(to)
        .bind(from)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
        if mutual.is_some() {
            let (user_a, user_b) = friendship_key(from, to);
            sqlx::query(
                "INSERT INTO friendships (user_a, user_b) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(user_a)
            .bind(user_b)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
            tx.commit().await.map_err(db_error)?;
            return Ok(RequestOutcome::Accepted);
        }

        let request_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO friend_requests (id, from_user_id, to_user_id) VALUES ($1, $2, $3)",
        )
        .bind(request_id)
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if is_unique_violation(&error) {
                AppError::Conflict
            } else {
                db_error(error)
            }
        })?;
        tx.commit().await.map_err(db_error)?;
        Ok(RequestOutcome::Pending(request_id))
    }

    async fn incoming_requests(&self, user_id: Uuid) -> Result<Vec<FriendRequest>, AppError> {
        let rows = sqlx::query(
            "SELECT id, from_user_id, to_user_id, created_at FROM friend_requests
             WHERE to_user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(rows.iter().map(friend_request_from_row).collect())
    }

    async fn respond(
        &self,
        request_id: Uuid,
        user_id: Uuid,
        accept: bool,
    ) -> Result<FriendRequest, AppError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let row = sqlx::query(
            "DELETE FROM friend_requests WHERE id = $1 AND to_user_id = $2
             RETURNING id, from_user_id, to_user_id, created_at",
        )
        .bind(request_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or(AppError::NotFound)?;
        let request = friend_request_from_row(&row);
        if accept {
            let (user_a, user_b) = friendship_key(request.from_user_id, request.to_user_id);
            sqlx::query(
                "INSERT INTO friendships (user_a, user_b) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(user_a)
            .bind(user_b)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(request)
    }

    async fn friends_of(&self, user_id: Uuid) -> Result<Vec<Uuid>, AppError> {
        let rows = sqlx::query(
            "SELECT user_b AS friend_id FROM friendships WHERE user_a = $1
             UNION ALL
             SELECT user_a AS friend_id FROM friendships WHERE user_b = $1",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(rows.iter().map(|row| row.get("friend_id")).collect())
    }
}

// ---------------------------------------------------------------------------
// Chat (conversations + messages)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PgChatStore {
    pool: PgPool,
}

impl PgChatStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn message_select(where_clause: &str, order: &str) -> String {
        format!(
            "SELECT m.id, m.conversation_id, m.sender_id, m.kind, m.body, m.attachment_id,
                    m.duration_ms, m.pinned, m.sent_at, m.deleted_for_everyone,
                    r.id AS reply_id, r.sender_id AS reply_sender_id, r.kind AS reply_kind,
                    r.body AS reply_body, r.deleted_for_everyone AS reply_deleted,
                    ARRAY(SELECT cm.user_id FROM conversation_members cm
                          WHERE cm.conversation_id = m.conversation_id
                            AND cm.user_id <> m.sender_id
                            AND cm.last_read_at IS NOT NULL
                            AND cm.last_read_at >= m.sent_at) AS read_by
             FROM messages m
             LEFT JOIN messages r ON r.id = m.reply_to
             WHERE {where_clause} {order}"
        )
    }

    async fn rows_to_views(&self, rows: Vec<PgRow>) -> Result<Vec<MessageView>, AppError> {
        let ids: Vec<Uuid> = rows.iter().map(|row| row.get("id")).collect();
        let reaction_rows = sqlx::query(
            "SELECT message_id, emoji, user_id FROM message_reactions
             WHERE message_id = ANY($1) ORDER BY emoji, created_at",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        let mut reactions: HashMap<Uuid, Vec<ReactionView>> = HashMap::new();
        for row in &reaction_rows {
            let message_id: Uuid = row.get("message_id");
            let emoji: String = row.get("emoji");
            let user_id: Uuid = row.get("user_id");
            let entry = reactions.entry(message_id).or_default();
            if let Some(view) = entry.iter_mut().find(|view| view.emoji == emoji) {
                view.user_ids.push(user_id);
            } else {
                entry.push(ReactionView {
                    emoji,
                    user_ids: vec![user_id],
                });
            }
        }

        rows.iter()
            .map(|row| {
                let id: Uuid = row.get("id");
                let deleted: bool = row.get("deleted_for_everyone");
                let reply_to = row
                    .get::<Option<Uuid>, _>("reply_id")
                    .map(|reply_id| -> Result<ReplyPreview, AppError> {
                        let reply_deleted: bool = row.get("reply_deleted");
                        let reply_body: String = row.get("reply_body");
                        Ok(ReplyPreview {
                            id: reply_id,
                            sender_id: row.get("reply_sender_id"),
                            kind: MessageKind::parse(row.get("reply_kind"))?,
                            body: if reply_deleted {
                                String::new()
                            } else {
                                truncate_preview(&reply_body)
                            },
                            deleted: reply_deleted,
                        })
                    })
                    .transpose()?;
                Ok(MessageView {
                    id,
                    conversation_id: row.get("conversation_id"),
                    sender_id: row.get("sender_id"),
                    kind: MessageKind::parse(row.get("kind"))?,
                    body: if deleted {
                        String::new()
                    } else {
                        row.get("body")
                    },
                    attachment_id: if deleted {
                        None
                    } else {
                        row.get("attachment_id")
                    },
                    duration_ms: if deleted {
                        None
                    } else {
                        row.get("duration_ms")
                    },
                    reply_to,
                    reactions: reactions.remove(&id).unwrap_or_default(),
                    pinned: row.get("pinned"),
                    read_by: row.get("read_by"),
                    sent_at: row.get("sent_at"),
                    deleted,
                })
            })
            .collect()
    }

    async fn view_by_id(&self, message_id: Uuid) -> Result<MessageView, AppError> {
        let rows = sqlx::query(&Self::message_select("m.id = $1", ""))
            .bind(message_id)
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?;
        self.rows_to_views(rows)
            .await?
            .into_iter()
            .next()
            .ok_or(AppError::NotFound)
    }

    async fn require_member(&self, conversation_id: Uuid, user: Uuid) -> Result<(), AppError> {
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE id = $1) AS exists_conv,
                    EXISTS(SELECT 1 FROM conversation_members WHERE conversation_id = $1 AND user_id = $2) AS member",
        )
        .bind(conversation_id)
        .bind(user)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)?;
        if !row.get::<bool, _>("exists_conv") {
            return Err(AppError::NotFound);
        }
        if !row.get::<bool, _>("member") {
            return Err(AppError::Forbidden);
        }
        Ok(())
    }

    /// Returns the conversation id of a message the user belongs to.
    async fn require_member_of_message(
        &self,
        message_id: Uuid,
        user: Uuid,
    ) -> Result<Uuid, AppError> {
        let row = sqlx::query(
            "SELECT m.conversation_id,
                    EXISTS(SELECT 1 FROM conversation_members cm
                           WHERE cm.conversation_id = m.conversation_id AND cm.user_id = $2) AS member
             FROM messages m WHERE m.id = $1",
        )
        .bind(message_id)
        .bind(user)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?
        .ok_or(AppError::NotFound)?;
        if !row.get::<bool, _>("member") {
            return Err(AppError::Forbidden);
        }
        Ok(row.get("conversation_id"))
    }

    async fn build_view(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<ConversationView, AppError> {
        let meta = sqlx::query(
            "SELECT c.kind, c.title, c.avatar_attachment_id, c.created_by, cm.last_read_at
             FROM conversations c
             JOIN conversation_members cm ON cm.conversation_id = c.id AND cm.user_id = $2
             WHERE c.id = $1",
        )
        .bind(conversation_id)
        .bind(user)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?
        .ok_or(AppError::NotFound)?;
        let last_read_at: Option<DateTime<Utc>> = meta.get("last_read_at");

        let member_ids = self.members(conversation_id).await?;

        let last_rows = sqlx::query(&Self::message_select(
            "m.conversation_id = $1
             AND NOT EXISTS (SELECT 1 FROM message_deletions d WHERE d.message_id = m.id AND d.user_id = $2)",
            "ORDER BY m.sent_at DESC, m.id DESC LIMIT 1",
        ))
        .bind(conversation_id)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        let last_message = self.rows_to_views(last_rows).await?.into_iter().next();

        let unread_row = sqlx::query(
            "SELECT count(*) AS unread FROM messages m
             WHERE m.conversation_id = $1 AND m.sender_id <> $2 AND NOT m.deleted_for_everyone
               AND ($3::timestamptz IS NULL OR m.sent_at > $3)
               AND NOT EXISTS (SELECT 1 FROM message_deletions d WHERE d.message_id = m.id AND d.user_id = $2)",
        )
        .bind(conversation_id)
        .bind(user)
        .bind(last_read_at)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)?;
        let unread: i64 = unread_row.get("unread");

        Ok(ConversationView {
            id: conversation_id,
            kind: ConversationKind::parse(meta.get("kind"))?,
            title: meta.get("title"),
            avatar_attachment_id: meta.get("avatar_attachment_id"),
            created_by: meta.get("created_by"),
            member_ids,
            unread_count: unread.max(0) as usize,
            last_message,
        })
    }
}

#[async_trait]
impl ChatStore for PgChatStore {
    async fn ensure_direct(&self, a: Uuid, b: Uuid) -> Result<Uuid, AppError> {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        if let Some(row) = sqlx::query(
            "SELECT conversation_id FROM direct_conversations WHERE user_a = $1 AND user_b = $2",
        )
        .bind(lo)
        .bind(hi)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?
        {
            return Ok(row.get("conversation_id"));
        }

        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let conversation_id = Uuid::new_v4();
        sqlx::query("INSERT INTO conversations (id, kind) VALUES ($1, 'direct')")
            .bind(conversation_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let inserted = sqlx::query(
            "INSERT INTO direct_conversations (user_a, user_b, conversation_id)
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(lo)
        .bind(hi)
        .bind(conversation_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        if inserted.rows_affected() == 0 {
            // Lost a race; roll back and read the winner.
            tx.rollback().await.map_err(db_error)?;
            let row = sqlx::query(
                "SELECT conversation_id FROM direct_conversations WHERE user_a = $1 AND user_b = $2",
            )
            .bind(lo)
            .bind(hi)
            .fetch_one(&self.pool)
            .await
            .map_err(db_error)?;
            return Ok(row.get("conversation_id"));
        }
        sqlx::query(
            "INSERT INTO conversation_members (conversation_id, user_id, role)
             VALUES ($1, $2, 'member'), ($1, $3, 'member')",
        )
        .bind(conversation_id)
        .bind(a)
        .bind(b)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(conversation_id)
    }

    async fn create_group(
        &self,
        creator: Uuid,
        title: String,
        members: &[Uuid],
    ) -> Result<Uuid, AppError> {
        let title = validate_group_title(&title)?;
        let conversation_id = Uuid::new_v4();
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query(
            "INSERT INTO conversations (id, kind, title, created_by) VALUES ($1, 'group', $2, $3)",
        )
        .bind(conversation_id)
        .bind(&title)
        .bind(creator)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query("INSERT INTO conversation_members (conversation_id, user_id, role) VALUES ($1, $2, 'owner')")
            .bind(conversation_id)
            .bind(creator)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        for member in members {
            if *member == creator {
                continue;
            }
            sqlx::query(
                "INSERT INTO conversation_members (conversation_id, user_id, role)
                 VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING",
            )
            .bind(conversation_id)
            .bind(*member)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(conversation_id)
    }

    async fn conversations_for(&self, user: Uuid) -> Result<Vec<ConversationView>, AppError> {
        let rows =
            sqlx::query("SELECT conversation_id FROM conversation_members WHERE user_id = $1")
                .bind(user)
                .fetch_all(&self.pool)
                .await
                .map_err(db_error)?;
        let mut views = Vec::with_capacity(rows.len());
        for row in rows {
            views.push(self.build_view(row.get("conversation_id"), user).await?);
        }
        views.sort_by(|a, b| {
            let a_at = a.last_message.as_ref().map(|message| message.sent_at);
            let b_at = b.last_message.as_ref().map(|message| message.sent_at);
            b_at.cmp(&a_at)
        });
        Ok(views)
    }

    async fn conversation_for(&self, id: Uuid, user: Uuid) -> Result<ConversationView, AppError> {
        self.require_member(id, user).await?;
        self.build_view(id, user).await
    }

    async fn members(&self, id: Uuid) -> Result<Vec<Uuid>, AppError> {
        let rows = sqlx::query(
            "SELECT user_id FROM conversation_members WHERE conversation_id = $1 ORDER BY position",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(rows.iter().map(|row| row.get("user_id")).collect())
    }

    async fn is_member(&self, id: Uuid, user: Uuid) -> Result<bool, AppError> {
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM conversation_members WHERE conversation_id = $1 AND user_id = $2) AS member",
        )
        .bind(id)
        .bind(user)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(row.get("member"))
    }

    async fn add_member(&self, id: Uuid, new_user: Uuid) -> Result<(), AppError> {
        let kind = sqlx::query("SELECT kind FROM conversations WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?
            .ok_or(AppError::NotFound)?;
        if ConversationKind::parse(kind.get("kind"))? != ConversationKind::Group {
            return Err(AppError::BadRequest("not a group conversation".to_owned()));
        }
        let inserted = sqlx::query(
            "INSERT INTO conversation_members (conversation_id, user_id, role)
             VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(new_user)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if inserted.rows_affected() == 0 {
            return Err(AppError::Conflict);
        }
        Ok(())
    }

    async fn remove_member(&self, id: Uuid, actor: Uuid, target: Uuid) -> Result<(), AppError> {
        let row = sqlx::query(
            "SELECT c.kind, (SELECT role FROM conversation_members WHERE conversation_id = $1 AND user_id = $2) AS actor_role
             FROM conversations c WHERE c.id = $1",
        )
        .bind(id)
        .bind(actor)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?
        .ok_or(AppError::NotFound)?;
        if ConversationKind::parse(row.get("kind"))? != ConversationKind::Group {
            return Err(AppError::BadRequest("not a group conversation".to_owned()));
        }
        let actor_role: Option<String> = row.get("actor_role");
        let is_owner = actor_role.as_deref().map(MemberRole::parse) == Some(MemberRole::Owner);
        if actor != target && !is_owner {
            return Err(AppError::Forbidden);
        }
        let deleted = sqlx::query(
            "DELETE FROM conversation_members WHERE conversation_id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(target)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if deleted.rows_affected() == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    async fn append(
        &self,
        conversation_id: Uuid,
        sender: Uuid,
        draft: NewMessage,
    ) -> Result<MessageView, AppError> {
        let draft = prepare_message(draft)?;
        self.require_member(conversation_id, sender).await?;
        if let Some(reply_id) = draft.reply_to {
            let row = sqlx::query(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id = $1 AND conversation_id = $2) AS found",
            )
            .bind(reply_id)
            .bind(conversation_id)
            .fetch_one(&self.pool)
            .await
            .map_err(db_error)?;
            if !row.get::<bool, _>("found") {
                return Err(AppError::BadRequest(
                    "replied message is not in this conversation".to_owned(),
                ));
            }
        }
        let message_id = Uuid::new_v4();
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query(
            "INSERT INTO messages (id, conversation_id, sender_id, kind, body, attachment_id, duration_ms, reply_to)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(message_id)
        .bind(conversation_id)
        .bind(sender)
        .bind(draft.kind.as_str())
        .bind(&draft.body)
        .bind(draft.attachment_id)
        .bind(draft.duration_ms)
        .bind(draft.reply_to)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        sqlx::query(
            "UPDATE conversation_members SET last_read_at = now() WHERE conversation_id = $1 AND user_id = $2",
        )
        .bind(conversation_id)
        .bind(sender)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        self.view_by_id(message_id).await
    }

    async fn history(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<Vec<MessageView>, AppError> {
        self.require_member(conversation_id, user).await?;
        let rows = sqlx::query(&Self::message_select(
            "m.conversation_id = $1
             AND NOT EXISTS (SELECT 1 FROM message_deletions d WHERE d.message_id = m.id AND d.user_id = $2)",
            "ORDER BY m.sent_at, m.id",
        ))
        .bind(conversation_id)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        self.rows_to_views(rows).await
    }

    async fn search(
        &self,
        conversation_id: Uuid,
        user: Uuid,
        query: &str,
    ) -> Result<Vec<MessageView>, AppError> {
        self.require_member(conversation_id, user).await?;
        let needle = query.trim();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = format!(
            "%{}%",
            needle
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = sqlx::query(&Self::message_select(
            "m.conversation_id = $1 AND m.kind = 'text' AND NOT m.deleted_for_everyone
             AND m.body ILIKE $3
             AND NOT EXISTS (SELECT 1 FROM message_deletions d WHERE d.message_id = m.id AND d.user_id = $2)",
            "ORDER BY m.sent_at, m.id",
        ))
        .bind(conversation_id)
        .bind(user)
        .bind(pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        self.rows_to_views(rows).await
    }

    async fn pinned(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<Vec<MessageView>, AppError> {
        self.require_member(conversation_id, user).await?;
        let rows = sqlx::query(&Self::message_select(
            "m.conversation_id = $1 AND m.pinned AND NOT m.deleted_for_everyone
             AND NOT EXISTS (SELECT 1 FROM message_deletions d WHERE d.message_id = m.id AND d.user_id = $2)",
            "ORDER BY m.sent_at, m.id",
        ))
        .bind(conversation_id)
        .bind(user)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        self.rows_to_views(rows).await
    }

    async fn mark_read(
        &self,
        conversation_id: Uuid,
        user: Uuid,
    ) -> Result<DateTime<Utc>, AppError> {
        let row = sqlx::query(
            "UPDATE conversation_members SET last_read_at = now()
             WHERE conversation_id = $1 AND user_id = $2 RETURNING last_read_at",
        )
        .bind(conversation_id)
        .bind(user)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?
        .ok_or(AppError::Forbidden)?;
        Ok(row.get("last_read_at"))
    }

    async fn delete_for_me(&self, message_id: Uuid, user: Uuid) -> Result<(), AppError> {
        self.require_member_of_message(message_id, user).await?;
        sqlx::query(
            "INSERT INTO message_deletions (message_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(message_id)
        .bind(user)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_for_everyone(
        &self,
        message_id: Uuid,
        user: Uuid,
    ) -> Result<(MessageView, Option<Uuid>), AppError> {
        self.require_member_of_message(message_id, user).await?;
        let row = sqlx::query("SELECT sender_id, attachment_id FROM messages WHERE id = $1")
            .bind(message_id)
            .fetch_one(&self.pool)
            .await
            .map_err(db_error)?;
        if row.get::<Uuid, _>("sender_id") != user {
            return Err(AppError::Forbidden);
        }
        let attachment_id: Option<Uuid> = row.get("attachment_id");
        sqlx::query(
            "UPDATE messages SET deleted_for_everyone = TRUE, body = '', pinned = FALSE, duration_ms = NULL WHERE id = $1",
        )
        .bind(message_id)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        sqlx::query("DELETE FROM message_reactions WHERE message_id = $1")
            .bind(message_id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok((self.view_by_id(message_id).await?, attachment_id))
    }

    async fn toggle_reaction(
        &self,
        message_id: Uuid,
        user: Uuid,
        emoji: &str,
    ) -> Result<ReactionResult, AppError> {
        crate::chat::validate_reaction(emoji)?;
        self.require_member_of_message(message_id, user).await?;
        let tombstoned = sqlx::query("SELECT deleted_for_everyone FROM messages WHERE id = $1")
            .bind(message_id)
            .fetch_one(&self.pool)
            .await
            .map_err(db_error)?;
        if tombstoned.get::<bool, _>("deleted_for_everyone") {
            return Err(AppError::BadRequest(
                "cannot react to a deleted message".to_owned(),
            ));
        }
        let removed = sqlx::query(
            "DELETE FROM message_reactions WHERE message_id = $1 AND user_id = $2 AND emoji = $3",
        )
        .bind(message_id)
        .bind(user)
        .bind(emoji)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        let added = if removed.rows_affected() == 0 {
            sqlx::query(
                "INSERT INTO message_reactions (message_id, user_id, emoji) VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(message_id)
            .bind(user)
            .bind(emoji)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
            true
        } else {
            false
        };
        Ok(ReactionResult {
            view: self.view_by_id(message_id).await?,
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
        self.require_member_of_message(message_id, user).await?;
        let updated = sqlx::query(
            "UPDATE messages SET pinned = $2 WHERE id = $1 AND NOT deleted_for_everyone",
        )
        .bind(message_id)
        .bind(pinned)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if updated.rows_affected() == 0 {
            return Err(AppError::BadRequest(
                "cannot pin a deleted message".to_owned(),
            ));
        }
        self.view_by_id(message_id).await
    }

    async fn attachment_visible(&self, attachment_id: Uuid, user: Uuid) -> Result<bool, AppError> {
        let row = sqlx::query(
            "SELECT EXISTS(
                SELECT 1 FROM messages m
                JOIN conversation_members cm ON cm.conversation_id = m.conversation_id AND cm.user_id = $2
                WHERE m.attachment_id = $1
             ) AS found",
        )
        .bind(attachment_id)
        .bind(user)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(row.get("found"))
    }
}

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PgAttachmentStore {
    pool: PgPool,
}

impl PgAttachmentStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AttachmentStore for PgAttachmentStore {
    async fn store(&self, owner_id: Uuid, bytes: Bytes) -> Result<(Uuid, String), AppError> {
        let content_type = validate_media(&bytes)?;
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO attachments (id, owner_id, content_type, bytes) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(owner_id)
        .bind(content_type)
        .bind(bytes.as_ref())
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok((id, content_type.to_owned()))
    }

    async fn mark_used(&self, id: Uuid, owner_id: Uuid) -> Result<(), AppError> {
        let row = sqlx::query("SELECT owner_id, used FROM attachments WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?
            .ok_or(AppError::NotFound)?;
        if row.get::<Uuid, _>("owner_id") != owner_id {
            return Err(AppError::NotFound);
        }
        if row.get::<bool, _>("used") {
            return Err(AppError::Conflict);
        }
        let updated = sqlx::query("UPDATE attachments SET used = TRUE WHERE id = $1 AND NOT used")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        if updated.rows_affected() == 1 {
            Ok(())
        } else {
            Err(AppError::Conflict)
        }
    }

    async fn is_owner(&self, id: Uuid, user_id: Uuid) -> Result<bool, AppError> {
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM attachments WHERE id = $1 AND owner_id = $2) AS found",
        )
        .bind(id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(row.get("found"))
    }

    async fn bytes(&self, id: Uuid) -> Result<(String, Bytes), AppError> {
        let row = sqlx::query("SELECT content_type, bytes FROM attachments WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?
            .ok_or(AppError::NotFound)?;
        let bytes: Vec<u8> = row.get("bytes");
        Ok((row.get("content_type"), Bytes::from(bytes)))
    }

    async fn remove(&self, id: Uuid) -> Result<(), AppError> {
        sqlx::query("DELETE FROM attachments WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests against a real Postgres database, skipped unless
    //! TEST_DATABASE_URL is set.

    use super::*;
    use crate::chat::{MessageKind, NewMessage};
    use sqlx::PgPool;

    async fn pool() -> Option<PgPool> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        let pool = PgPool::connect(&url)
            .await
            .expect("test database reachable");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations apply");
        Some(pool)
    }

    fn phone() -> String {
        format!("+1{:010}", Uuid::new_v4().as_u128() % 10_000_000_000)
    }

    async fn user(pool: &PgPool, name: &str) -> Uuid {
        PgUserStore::new(pool.clone())
            .create_user(phone(), name.to_owned(), "hash".to_owned())
            .await
            .unwrap()
            .id
    }

    async fn befriend(pool: &PgPool, a: Uuid, b: Uuid) {
        let friends = PgFriendStore::new(pool.clone());
        let RequestOutcome::Pending(id) = friends.send_request(a, b).await.unwrap() else {
            panic!("pending");
        };
        friends.respond(id, b, true).await.unwrap();
    }

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
    async fn pg_direct_messaging_receipts_and_unread() {
        let Some(pool) = pool().await else { return };
        let chat = PgChatStore::new(pool.clone());
        let alice = user(&pool, "Alice").await;
        let bob = user(&pool, "Bob").await;
        befriend(&pool, alice, bob).await;

        let convo = chat.ensure_direct(alice, bob).await.unwrap();
        assert_eq!(chat.ensure_direct(bob, alice).await.unwrap(), convo);

        chat.append(convo, alice, text("hi")).await.unwrap();
        chat.append(convo, alice, text("there")).await.unwrap();
        assert_eq!(
            chat.conversation_for(convo, alice)
                .await
                .unwrap()
                .unread_count,
            0
        );
        assert_eq!(
            chat.conversation_for(convo, bob)
                .await
                .unwrap()
                .unread_count,
            2
        );

        chat.mark_read(convo, bob).await.unwrap();
        assert_eq!(
            chat.conversation_for(convo, bob)
                .await
                .unwrap()
                .unread_count,
            0
        );
        let history = chat.history(convo, alice).await.unwrap();
        assert!(history.iter().all(|m| m.read_by == vec![bob]));

        // Non-member rejected.
        let mallory = user(&pool, "Mallory").await;
        assert!(chat.history(convo, mallory).await.is_err());
        assert!(chat.append(convo, mallory, text("x")).await.is_err());
    }

    #[tokio::test]
    async fn pg_group_pin_search_reactions_replies_delete() {
        let Some(pool) = pool().await else { return };
        let chat = PgChatStore::new(pool.clone());
        let owner = user(&pool, "Owner").await;
        let a = user(&pool, "Aa").await;
        let b = user(&pool, "Bb").await;

        let convo = chat
            .create_group(owner, "Team".to_owned(), &[a, b])
            .await
            .unwrap();
        assert_eq!(chat.members(convo).await.unwrap().len(), 3);

        let m1 = chat
            .append(convo, owner, text("buy MILK today"))
            .await
            .unwrap();
        let reply = chat
            .append(
                convo,
                a,
                NewMessage {
                    reply_to: Some(m1.id),
                    ..text("ok")
                },
            )
            .await
            .unwrap();
        assert_eq!(reply.reply_to.as_ref().unwrap().body, "buy MILK today");

        // group read_by accumulates
        chat.mark_read(convo, a).await.unwrap();
        chat.mark_read(convo, b).await.unwrap();
        let history = chat.history(convo, owner).await.unwrap();
        assert_eq!(history[0].read_by.len(), 2);

        // search case-insensitive
        let hits = chat.search(convo, b, "milk").await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, m1.id);

        // pin
        chat.set_pinned(m1.id, b, true).await.unwrap();
        assert_eq!(chat.pinned(convo, a).await.unwrap().len(), 1);

        // reactions
        let r = chat.toggle_reaction(m1.id, b, "👍").await.unwrap();
        assert!(r.added);
        assert!(
            chat.toggle_reaction(m1.id, user(&pool, "Out").await, "👍")
                .await
                .is_err()
        );
        assert!(chat.toggle_reaction(m1.id, b, "🤖").await.is_err());

        // membership management
        assert!(chat.remove_member(convo, a, b).await.is_err()); // non-owner can't remove other
        chat.remove_member(convo, b, b).await.unwrap(); // leave
        assert_eq!(chat.members(convo).await.unwrap().len(), 2);

        // delete-for-everyone: sender only, clears pin/search/reactions
        assert!(chat.delete_for_everyone(m1.id, a).await.is_err());
        let (tombstone, _) = chat.delete_for_everyone(m1.id, owner).await.unwrap();
        assert!(tombstone.deleted);
        assert!(chat.search(convo, a, "milk").await.unwrap().is_empty());
        assert!(chat.pinned(convo, a).await.unwrap().is_empty());
        let reply_view = chat.history(convo, a).await.unwrap();
        let rv = reply_view.iter().find(|m| m.id == reply.id).unwrap();
        assert!(rv.reply_to.as_ref().unwrap().deleted);
    }

    #[tokio::test]
    async fn pg_avatar_and_attachment_visibility() {
        let Some(pool) = pool().await else { return };
        let users = PgUserStore::new(pool.clone());
        let attachments = PgAttachmentStore::new(pool.clone());
        let chat = PgChatStore::new(pool.clone());
        let alice = user(&pool, "Alice").await;
        let bob = user(&pool, "Bob").await;
        let mallory = user(&pool, "Mallory").await;
        befriend(&pool, alice, bob).await;
        let convo = chat.ensure_direct(alice, bob).await.unwrap();

        let png = Bytes::from_static(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]);
        let (att, _) = attachments.store(alice, png.clone()).await.unwrap();

        // before send: only owner; not visible to bob via chat
        assert!(!chat.attachment_visible(att, bob).await.unwrap());
        attachments.mark_used(att, alice).await.unwrap();
        chat.append(
            convo,
            alice,
            NewMessage {
                kind: MessageKind::Image,
                body: String::new(),
                attachment_id: Some(att),
                duration_ms: None,
                reply_to: None,
            },
        )
        .await
        .unwrap();
        assert!(chat.attachment_visible(att, bob).await.unwrap());
        assert!(!chat.attachment_visible(att, mallory).await.unwrap());

        // avatar
        let (att2, _) = attachments.store(alice, png).await.unwrap();
        let (prev, updated) = users.set_avatar(alice, att2).await.unwrap();
        assert!(prev.is_none());
        assert_eq!(updated.avatar_attachment_id, Some(att2));
        assert!(users.avatar_in_use(att2).await);
    }
}
