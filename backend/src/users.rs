use crate::error::AppError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: Uuid,
    pub phone: String,
    pub display_name: String,
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
