//! Postgres implementations of the storage traits, used when DATABASE_URL is
//! configured. Queries are written so that authorization checks (participant,
//! sender-only, owner-only) mirror the in-memory stores exactly.

use crate::{
    attachments::{AttachmentStore, validate_image},
    error::AppError,
    friends::{FriendRequest, FriendStore, RequestOutcome, friendship_key},
    messages::{
        ConversationSummary, MessageKind, MessageStore, MessageView, ReactionToggle, ReactionView,
        ReplyPreview, truncate_preview, validate_body, validate_reaction,
    },
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
    matches!(
        error,
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505")
    )
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
        last_seen_at: row.get("last_seen_at"),
        created_at: row.get("created_at"),
    }
}

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
             RETURNING id, phone, display_name, last_seen_at, created_at",
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
        let row = sqlx::query(
            "SELECT id, phone, display_name, password_hash, last_seen_at, created_at
             FROM users WHERE phone = $1",
        )
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
        let row = sqlx::query(
            "SELECT id, phone, display_name, last_seen_at, created_at FROM users WHERE id = $1",
        )
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
                "INSERT INTO friendships (user_a, user_b) VALUES ($1, $2)
                 ON CONFLICT DO NOTHING",
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
                "INSERT INTO friendships (user_a, user_b) VALUES ($1, $2)
                 ON CONFLICT DO NOTHING",
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
// Messages
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PgMessageStore {
    pool: PgPool,
}

impl PgMessageStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Loads full message views (with reply previews and reactions) for the
    /// given WHERE clause over `m`. `r` is the optional reply-target message.
    async fn fetch_views(
        &self,
        where_clause: &str,
        binds: &[Uuid],
        order_desc_limit_one: bool,
    ) -> Result<Vec<MessageView>, AppError> {
        let order = if order_desc_limit_one {
            "ORDER BY m.sent_at DESC, m.id DESC LIMIT 1"
        } else {
            "ORDER BY m.sent_at, m.id"
        };
        let sql = format!(
            "SELECT m.id, m.sender_id, m.recipient_id, m.kind, m.body, m.attachment_id,
                    m.sent_at, m.delivered_at, m.read_at, m.deleted_for_everyone,
                    r.id AS reply_id, r.sender_id AS reply_sender_id, r.kind AS reply_kind,
                    r.body AS reply_body, r.deleted_for_everyone AS reply_deleted
             FROM messages m
             LEFT JOIN messages r ON r.id = m.reply_to
             WHERE {where_clause} {order}"
        );
        let mut query = sqlx::query(&sql);
        for bind in binds {
            query = query.bind(bind);
        }
        let rows = query.fetch_all(&self.pool).await.map_err(db_error)?;

        let message_ids: Vec<Uuid> = rows.iter().map(|row| row.get("id")).collect();
        let reaction_rows = sqlx::query(
            "SELECT message_id, emoji, user_id FROM message_reactions
             WHERE message_id = ANY($1) ORDER BY emoji, created_at",
        )
        .bind(&message_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        let mut reactions_by_message: HashMap<Uuid, Vec<ReactionView>> = HashMap::new();
        for row in &reaction_rows {
            let message_id: Uuid = row.get("message_id");
            let emoji: String = row.get("emoji");
            let user_id: Uuid = row.get("user_id");
            let reactions = reactions_by_message.entry(message_id).or_default();
            if let Some(existing) = reactions.iter_mut().find(|view| view.emoji == emoji) {
                existing.user_ids.push(user_id);
            } else {
                reactions.push(ReactionView {
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
                    sender_id: row.get("sender_id"),
                    recipient_id: row.get("recipient_id"),
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
                    reply_to,
                    reactions: reactions_by_message.remove(&id).unwrap_or_default(),
                    sent_at: row.get("sent_at"),
                    delivered_at: row.get("delivered_at"),
                    read_at: row.get("read_at"),
                    deleted,
                })
            })
            .collect()
    }

    async fn fetch_view(&self, message_id: Uuid) -> Result<MessageView, AppError> {
        self.fetch_views("m.id = $1", &[message_id], false)
            .await?
            .into_iter()
            .next()
            .ok_or(AppError::NotFound)
    }

    /// Validates the reply target (same conversation) and inserts the row.
    async fn insert_message(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        kind: MessageKind,
        body: String,
        attachment_id: Option<Uuid>,
        reply_to: Option<Uuid>,
    ) -> Result<MessageView, AppError> {
        if let Some(reply_id) = reply_to {
            let row = sqlx::query(
                "SELECT EXISTS(
                    SELECT 1 FROM messages
                    WHERE id = $1
                      AND LEAST(sender_id, recipient_id) = LEAST($2::uuid, $3::uuid)
                      AND GREATEST(sender_id, recipient_id) = GREATEST($2::uuid, $3::uuid)
                 ) AS found",
            )
            .bind(reply_id)
            .bind(sender_id)
            .bind(recipient_id)
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
        sqlx::query(
            "INSERT INTO messages (id, sender_id, recipient_id, kind, body, attachment_id, reply_to)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(message_id)
        .bind(sender_id)
        .bind(recipient_id)
        .bind(kind.as_str())
        .bind(&body)
        .bind(attachment_id)
        .bind(reply_to)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        self.fetch_view(message_id).await
    }

    /// Returns (sender_id, recipient_id, deleted, attachment_id) after
    /// verifying that `user_id` participates in the message's conversation.
    async fn load_participant_checked(
        &self,
        message_id: Uuid,
        user_id: Uuid,
    ) -> Result<(Uuid, Uuid, bool, Option<Uuid>), AppError> {
        let row = sqlx::query(
            "SELECT sender_id, recipient_id, deleted_for_everyone, attachment_id
             FROM messages WHERE id = $1",
        )
        .bind(message_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?
        .ok_or(AppError::NotFound)?;
        let sender_id: Uuid = row.get("sender_id");
        let recipient_id: Uuid = row.get("recipient_id");
        if sender_id != user_id && recipient_id != user_id {
            return Err(AppError::NotFound);
        }
        Ok((
            sender_id,
            recipient_id,
            row.get("deleted_for_everyone"),
            row.get("attachment_id"),
        ))
    }
}

const NOT_DELETED_FOR_USER: &str = "NOT EXISTS (
    SELECT 1 FROM message_deletions d WHERE d.message_id = m.id AND d.user_id = $3
)";

#[async_trait]
impl MessageStore for PgMessageStore {
    async fn append_text(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        body: String,
        reply_to: Option<Uuid>,
    ) -> Result<MessageView, AppError> {
        let body = validate_body(&body)?;
        self.insert_message(
            sender_id,
            recipient_id,
            MessageKind::Text,
            body,
            None,
            reply_to,
        )
        .await
    }

    async fn append_image(
        &self,
        sender_id: Uuid,
        recipient_id: Uuid,
        attachment_id: Uuid,
        reply_to: Option<Uuid>,
    ) -> Result<MessageView, AppError> {
        self.insert_message(
            sender_id,
            recipient_id,
            MessageKind::Image,
            String::new(),
            Some(attachment_id),
            reply_to,
        )
        .await
    }

    async fn history(&self, user_id: Uuid, peer_id: Uuid) -> Result<Vec<MessageView>, AppError> {
        let clause = format!(
            "LEAST(m.sender_id, m.recipient_id) = LEAST($1::uuid, $2::uuid)
             AND GREATEST(m.sender_id, m.recipient_id) = GREATEST($1::uuid, $2::uuid)
             AND {NOT_DELETED_FOR_USER}"
        );
        self.fetch_views(&clause, &[user_id, peer_id, user_id], false)
            .await
    }

    async fn delete_for_me(&self, message_id: Uuid, user_id: Uuid) -> Result<(), AppError> {
        self.load_participant_checked(message_id, user_id).await?;
        sqlx::query(
            "INSERT INTO message_deletions (message_id, user_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(message_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn delete_for_everyone(
        &self,
        message_id: Uuid,
        user_id: Uuid,
    ) -> Result<(MessageView, Option<Uuid>), AppError> {
        let (sender_id, _, _, attachment_id) =
            self.load_participant_checked(message_id, user_id).await?;
        if sender_id != user_id {
            return Err(AppError::Forbidden);
        }
        sqlx::query("UPDATE messages SET deleted_for_everyone = TRUE, body = '' WHERE id = $1")
            .bind(message_id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM message_reactions WHERE message_id = $1")
            .bind(message_id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        let view = self.fetch_view(message_id).await?;
        Ok((view, attachment_id))
    }

    async fn mark_delivered(
        &self,
        message_ids: &[Uuid],
        recipient_id: Uuid,
    ) -> Result<Vec<(Uuid, Uuid)>, AppError> {
        let rows = sqlx::query(
            "UPDATE messages SET delivered_at = now()
             WHERE id = ANY($1) AND recipient_id = $2 AND delivered_at IS NULL
             RETURNING id, sender_id",
        )
        .bind(message_ids)
        .bind(recipient_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(rows
            .iter()
            .map(|row| (row.get("id"), row.get("sender_id")))
            .collect())
    }

    async fn mark_read(&self, user_id: Uuid, peer_id: Uuid) -> Result<Vec<Uuid>, AppError> {
        let rows = sqlx::query(
            "UPDATE messages
             SET read_at = now(), delivered_at = COALESCE(delivered_at, now())
             WHERE sender_id = $1 AND recipient_id = $2 AND read_at IS NULL
             RETURNING id",
        )
        .bind(peer_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(rows.iter().map(|row| row.get("id")).collect())
    }

    async fn toggle_reaction(
        &self,
        message_id: Uuid,
        user_id: Uuid,
        emoji: &str,
    ) -> Result<ReactionToggle, AppError> {
        validate_reaction(emoji)?;
        let (sender_id, recipient_id, deleted, _) =
            self.load_participant_checked(message_id, user_id).await?;
        if deleted {
            return Err(AppError::BadRequest(
                "cannot react to a deleted message".to_owned(),
            ));
        }
        let removed = sqlx::query(
            "DELETE FROM message_reactions
             WHERE message_id = $1 AND user_id = $2 AND emoji = $3",
        )
        .bind(message_id)
        .bind(user_id)
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
            .bind(user_id)
            .bind(emoji)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
            true
        } else {
            false
        };
        Ok(ReactionToggle {
            added,
            sender_id,
            recipient_id,
        })
    }

    async fn summary(&self, user_id: Uuid, peer_id: Uuid) -> Result<ConversationSummary, AppError> {
        let clause = format!(
            "LEAST(m.sender_id, m.recipient_id) = LEAST($1::uuid, $2::uuid)
             AND GREATEST(m.sender_id, m.recipient_id) = GREATEST($1::uuid, $2::uuid)
             AND {NOT_DELETED_FOR_USER}"
        );
        let last_message = self
            .fetch_views(&clause, &[user_id, peer_id, user_id], true)
            .await?
            .into_iter()
            .next();
        let row = sqlx::query(
            "SELECT count(*) AS unread FROM messages m
             WHERE m.sender_id = $2 AND m.recipient_id = $1
               AND m.read_at IS NULL AND NOT m.deleted_for_everyone
               AND NOT EXISTS (
                   SELECT 1 FROM message_deletions d
                   WHERE d.message_id = m.id AND d.user_id = $1
               )",
        )
        .bind(user_id)
        .bind(peer_id)
        .fetch_one(&self.pool)
        .await
        .map_err(db_error)?;
        let unread: i64 = row.get("unread");
        Ok(ConversationSummary {
            last_message,
            unread_count: unread.max(0) as usize,
        })
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
        let content_type = validate_image(&bytes)?;
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

    async fn attach(&self, id: Uuid, owner_id: Uuid, recipient_id: Uuid) -> Result<(), AppError> {
        let row = sqlx::query("SELECT owner_id, recipient_id FROM attachments WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?
            .ok_or(AppError::NotFound)?;
        if row.get::<Uuid, _>("owner_id") != owner_id {
            return Err(AppError::NotFound);
        }
        if row.get::<Option<Uuid>, _>("recipient_id").is_some() {
            return Err(AppError::Conflict);
        }
        let updated = sqlx::query(
            "UPDATE attachments SET recipient_id = $3
             WHERE id = $1 AND owner_id = $2 AND recipient_id IS NULL",
        )
        .bind(id)
        .bind(owner_id)
        .bind(recipient_id)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if updated.rows_affected() == 1 {
            Ok(())
        } else {
            Err(AppError::Conflict)
        }
    }

    async fn fetch(&self, id: Uuid, user_id: Uuid) -> Result<(String, Bytes), AppError> {
        let row = sqlx::query(
            "SELECT content_type, bytes FROM attachments
             WHERE id = $1 AND (owner_id = $2 OR recipient_id = $2)",
        )
        .bind(id)
        .bind(user_id)
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
    //! Integration tests against a real Postgres database. They are skipped
    //! when TEST_DATABASE_URL is not set, so `cargo test` stays runnable
    //! without local infrastructure.

    use super::{PgAttachmentStore, PgFriendStore, PgMessageStore, PgUserStore};
    use crate::{
        attachments::AttachmentStore,
        friends::{FriendStore, RequestOutcome},
        messages::MessageStore,
        users::UserStore,
    };
    use axum::body::Bytes;
    use sqlx::PgPool;
    use uuid::Uuid;

    async fn test_pool() -> Option<PgPool> {
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

    fn random_phone() -> String {
        let digits = Uuid::new_v4().as_u128() % 10_000_000_000;
        format!("+1{digits:010}")
    }

    async fn create_user(pool: &PgPool, name: &str) -> Uuid {
        PgUserStore::new(pool.clone())
            .create_user(random_phone(), name.to_owned(), "hash".to_owned())
            .await
            .expect("user created")
            .id
    }

    async fn make_friends(pool: &PgPool, a: Uuid, b: Uuid) {
        let friends = PgFriendStore::new(pool.clone());
        let RequestOutcome::Pending(request_id) = friends.send_request(a, b).await.unwrap() else {
            panic!("expected pending request");
        };
        friends.respond(request_id, b, true).await.unwrap();
    }

    fn png_bytes() -> Bytes {
        Bytes::from_static(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0])
    }

    #[tokio::test]
    async fn pg_user_store_roundtrip_and_conflicts() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let store = PgUserStore::new(pool);

        let phone = random_phone();
        let user = store
            .create_user(phone.clone(), "Alice".to_owned(), "hash".to_owned())
            .await
            .unwrap();
        assert!(
            store
                .create_user(phone.clone(), "Imposter".to_owned(), "hash".to_owned())
                .await
                .is_err()
        );

        let stored = store.find_by_phone(&phone).await.expect("found by phone");
        assert_eq!(stored.user.id, user.id);
        let found = store.find_by_id(user.id).await.expect("found by id");
        assert_eq!(found.phone, phone);
        assert!(found.last_seen_at.is_none());

        let now = chrono::Utc::now();
        store.set_last_seen(user.id, now).await;
        let found = store.find_by_id(user.id).await.unwrap();
        assert!(found.last_seen_at.is_some());
    }

    #[tokio::test]
    async fn pg_friend_flow_matches_in_memory_semantics() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let friends = PgFriendStore::new(pool.clone());
        let alice = create_user(&pool, "Alice").await;
        let bob = create_user(&pool, "Bob").await;
        let carol = create_user(&pool, "Carol").await;

        assert!(friends.send_request(alice, alice).await.is_err());

        let RequestOutcome::Pending(request_id) = friends.send_request(alice, bob).await.unwrap()
        else {
            panic!("expected pending");
        };
        // Duplicate send returns the same pending request.
        assert_eq!(
            friends.send_request(alice, bob).await.unwrap(),
            RequestOutcome::Pending(request_id)
        );
        assert!(!friends.are_friends(alice, bob).await.unwrap());
        // Only the addressee can respond.
        assert!(friends.respond(request_id, carol, true).await.is_err());
        friends.respond(request_id, bob, true).await.unwrap();
        assert!(friends.are_friends(alice, bob).await.unwrap());
        assert_eq!(friends.friends_of(alice).await.unwrap(), vec![bob]);
        // Already friends → conflict.
        assert!(friends.send_request(alice, bob).await.is_err());

        // Mutual requests auto-accept.
        friends.send_request(alice, carol).await.unwrap();
        assert_eq!(
            friends.send_request(carol, alice).await.unwrap(),
            RequestOutcome::Accepted
        );
        assert!(friends.are_friends(alice, carol).await.unwrap());
    }

    #[tokio::test]
    async fn pg_message_flow_receipts_reactions_replies_and_deletes() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let messages = PgMessageStore::new(pool.clone());
        let alice = create_user(&pool, "Alice").await;
        let bob = create_user(&pool, "Bob").await;
        make_friends(&pool, alice, bob).await;

        let first = messages
            .append_text(alice, bob, "hello".to_owned(), None)
            .await
            .unwrap();
        assert!(first.delivered_at.is_none() && first.read_at.is_none());

        // Reply with embedded preview; cross-conversation replies rejected.
        let reply = messages
            .append_text(bob, alice, "hi back".to_owned(), Some(first.id))
            .await
            .unwrap();
        let preview = reply.reply_to.clone().expect("preview");
        assert_eq!(preview.id, first.id);
        assert_eq!(preview.body, "hello");
        let outsider = create_user(&pool, "Mallory").await;
        make_friends(&pool, alice, outsider).await;
        let elsewhere = messages
            .append_text(alice, outsider, "psst".to_owned(), None)
            .await
            .unwrap();
        assert!(
            messages
                .append_text(bob, alice, "bad".to_owned(), Some(elsewhere.id))
                .await
                .is_err()
        );

        // Receipts.
        assert!(
            messages
                .mark_delivered(&[first.id], alice)
                .await
                .unwrap()
                .is_empty()
        );
        let updated = messages.mark_delivered(&[first.id], bob).await.unwrap();
        assert_eq!(updated, vec![(first.id, alice)]);
        let read_ids = messages.mark_read(bob, alice).await.unwrap();
        assert_eq!(read_ids, vec![first.id]);
        assert!(messages.mark_read(bob, alice).await.unwrap().is_empty());

        // Reactions toggle and authorization.
        let toggle = messages.toggle_reaction(first.id, bob, "👍").await.unwrap();
        assert!(toggle.added);
        assert!(
            messages
                .toggle_reaction(first.id, outsider, "👍")
                .await
                .is_err()
        );
        assert!(messages.toggle_reaction(first.id, bob, "🤖").await.is_err());
        let history = messages.history(alice, bob).await.unwrap();
        let first_view = history.iter().find(|m| m.id == first.id).unwrap();
        assert_eq!(first_view.reactions.len(), 1);
        assert_eq!(first_view.reactions[0].user_ids, vec![bob]);
        let toggle = messages.toggle_reaction(first.id, bob, "👍").await.unwrap();
        assert!(!toggle.added);

        // Summary: unread counts and last message.
        let summary = messages.summary(alice, bob).await.unwrap();
        assert_eq!(summary.unread_count, 1); // bob's reply is unread for alice
        assert_eq!(summary.last_message.unwrap().id, reply.id);

        // Delete for me hides only for that user.
        messages.delete_for_me(reply.id, alice).await.unwrap();
        assert!(messages.delete_for_me(reply.id, outsider).await.is_err());
        let alice_history = messages.history(alice, bob).await.unwrap();
        assert!(alice_history.iter().all(|m| m.id != reply.id));
        let bob_history = messages.history(bob, alice).await.unwrap();
        assert!(bob_history.iter().any(|m| m.id == reply.id));

        // Delete for everyone: sender only, tombstone, preview follows.
        assert!(messages.delete_for_everyone(first.id, bob).await.is_err());
        let (tombstone, _) = messages.delete_for_everyone(first.id, alice).await.unwrap();
        assert!(tombstone.deleted && tombstone.body.is_empty());
        assert!(messages.toggle_reaction(first.id, bob, "👍").await.is_err());
        let bob_history = messages.history(bob, alice).await.unwrap();
        let reply_view = bob_history.iter().find(|m| m.id == reply.id).unwrap();
        let preview = reply_view.reply_to.as_ref().unwrap();
        assert!(preview.deleted && preview.body.is_empty());
    }

    #[tokio::test]
    async fn pg_attachment_flow_and_image_purge() {
        let Some(pool) = test_pool().await else {
            return;
        };
        let attachments = PgAttachmentStore::new(pool.clone());
        let messages = PgMessageStore::new(pool.clone());
        let alice = create_user(&pool, "Alice").await;
        let bob = create_user(&pool, "Bob").await;
        let mallory = create_user(&pool, "Mallory").await;
        make_friends(&pool, alice, bob).await;

        assert!(
            attachments
                .store(alice, Bytes::from_static(b"not an image"))
                .await
                .is_err()
        );
        let (attachment_id, content_type) = attachments.store(alice, png_bytes()).await.unwrap();
        assert_eq!(content_type, "image/png");

        // Pre-send access and attach authorization.
        assert!(attachments.fetch(attachment_id, alice).await.is_ok());
        assert!(attachments.fetch(attachment_id, bob).await.is_err());
        assert!(attachments.attach(attachment_id, bob, alice).await.is_err());
        attachments.attach(attachment_id, alice, bob).await.unwrap();
        assert!(attachments.attach(attachment_id, alice, bob).await.is_err());
        assert!(attachments.fetch(attachment_id, bob).await.is_ok());
        assert!(attachments.fetch(attachment_id, mallory).await.is_err());

        // Image message + delete-for-everyone purges the attachment.
        let message = messages
            .append_image(alice, bob, attachment_id, None)
            .await
            .unwrap();
        assert_eq!(message.attachment_id, Some(attachment_id));
        let (tombstone, purged) = messages
            .delete_for_everyone(message.id, alice)
            .await
            .unwrap();
        assert!(tombstone.deleted && tombstone.attachment_id.is_none());
        assert_eq!(purged, Some(attachment_id));
        attachments.remove(attachment_id).await.unwrap();
        assert!(attachments.fetch(attachment_id, alice).await.is_err());
    }
}
