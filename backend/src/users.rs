use crate::{auth, error::AppError, state::AppState};
use async_trait::async_trait;
use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: Uuid,
    pub phone: String,
    pub display_name: String,
    /// Attachment id of the user's profile photo, if set.
    pub avatar_attachment_id: Option<Uuid>,
    /// Set when the user's last WebSocket connection closes. `None` for users
    /// who never connected (or, in the in-memory store, since last restart).
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct StoredUser {
    pub user: User,
    pub password_hash: String,
}

#[async_trait]
pub trait UserStore: Send + Sync {
    async fn create_user(
        &self,
        phone: String,
        display_name: String,
        password_hash: String,
    ) -> Result<User, AppError>;
    async fn find_by_phone(&self, phone: &str) -> Option<StoredUser>;
    async fn find_by_id(&self, id: Uuid) -> Option<User>;
    async fn set_last_seen(&self, id: Uuid, at: DateTime<Utc>);
    /// Sets the profile photo, returning the previous avatar attachment id (so
    /// the caller can purge it) and the updated user.
    async fn set_avatar(
        &self,
        id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(Option<Uuid>, User), AppError>;
    /// True if the attachment is currently in use as someone's avatar.
    async fn avatar_in_use(&self, attachment_id: Uuid) -> bool;
}

#[derive(Clone, Default)]
pub struct InMemoryUserStore {
    inner: Arc<RwLock<HashMap<Uuid, StoredUser>>>,
    phone_index: Arc<RwLock<HashMap<String, Uuid>>>,
}

#[async_trait]
impl UserStore for InMemoryUserStore {
    async fn create_user(
        &self,
        phone: String,
        display_name: String,
        password_hash: String,
    ) -> Result<User, AppError> {
        let normalized_phone = normalize_phone(&phone)?;
        let mut phone_index = self.phone_index.write().await;
        if phone_index.contains_key(&normalized_phone) {
            return Err(AppError::Conflict);
        }

        let user = User {
            id: Uuid::new_v4(),
            phone: normalized_phone.clone(),
            display_name,
            avatar_attachment_id: None,
            last_seen_at: None,
            created_at: Utc::now(),
        };
        phone_index.insert(normalized_phone, user.id);
        self.inner.write().await.insert(
            user.id,
            StoredUser {
                user: user.clone(),
                password_hash,
            },
        );
        Ok(user)
    }

    async fn find_by_phone(&self, phone: &str) -> Option<StoredUser> {
        let normalized_phone = normalize_phone(phone).ok()?;
        let phone_index = self.phone_index.read().await;
        let user_id = phone_index.get(&normalized_phone)?;
        self.inner.read().await.get(user_id).cloned()
    }

    async fn find_by_id(&self, id: Uuid) -> Option<User> {
        self.inner
            .read()
            .await
            .get(&id)
            .map(|stored| stored.user.clone())
    }

    async fn set_last_seen(&self, id: Uuid, at: DateTime<Utc>) {
        if let Some(stored) = self.inner.write().await.get_mut(&id) {
            stored.user.last_seen_at = Some(at);
        }
    }

    async fn set_avatar(
        &self,
        id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(Option<Uuid>, User), AppError> {
        let mut inner = self.inner.write().await;
        let stored = inner.get_mut(&id).ok_or(AppError::NotFound)?;
        let previous = stored.user.avatar_attachment_id.replace(attachment_id);
        Ok((previous, stored.user.clone()))
    }

    async fn avatar_in_use(&self, attachment_id: Uuid) -> bool {
        self.inner
            .read()
            .await
            .values()
            .any(|stored| stored.user.avatar_attachment_id == Some(attachment_id))
    }
}

pub fn normalize_phone(phone: &str) -> Result<String, AppError> {
    let trimmed = phone.trim();
    let valid = trimmed.starts_with('+')
        && trimmed.len() >= 8
        && trimmed.len() <= 16
        && trimmed[1..].chars().all(|ch| ch.is_ascii_digit());
    if valid {
        Ok(trimmed.to_owned())
    } else {
        Err(AppError::BadRequest(
            "phone must be E.164 format".to_owned(),
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct SetAvatarRequest {
    attachment_id: Uuid,
}

/// PUT /me/avatar — authenticated; sets the caller's profile photo to a
/// previously uploaded image attachment they own. The previous avatar's bytes
/// are purged.
pub async fn set_avatar(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Json(request): Json<SetAvatarRequest>,
) -> Result<Json<User>, AppError> {
    // The attachment must be an image owned by the caller.
    if !state
        .attachments
        .is_owner(request.attachment_id, user.id)
        .await?
    {
        return Err(AppError::NotFound);
    }
    let (content_type, _) = state.attachments.bytes(request.attachment_id).await?;
    if !content_type.starts_with("image/") {
        return Err(AppError::BadRequest("avatar must be an image".to_owned()));
    }
    state
        .attachments
        .mark_used(request.attachment_id, user.id)
        .await?;
    let (previous, updated) = state
        .users
        .set_avatar(user.id, request.attachment_id)
        .await?;
    if let Some(previous) = previous {
        let _ = state.attachments.remove(previous).await;
    }
    Ok(Json(updated))
}

#[cfg(test)]
mod tests {
    use super::normalize_phone;

    #[test]
    fn accepts_e164_phone_numbers() {
        assert_eq!(normalize_phone("+15550001111").unwrap(), "+15550001111");
    }

    #[test]
    fn rejects_non_e164_phone_numbers() {
        assert!(normalize_phone("555-000-1111").is_err());
        assert!(normalize_phone("+12").is_err());
        assert!(normalize_phone("+1555abc1111").is_err());
    }
}
