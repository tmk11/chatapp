use crate::{auth, error::AppError, state::AppState};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct Room {
    pub id: String,
    pub name: String,
    pub owner_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Default)]
pub struct InMemoryRoomStore {
    inner: Arc<RwLock<HashMap<String, Room>>>,
    name_index: Arc<RwLock<HashMap<String, String>>>,
}

impl InMemoryRoomStore {
    pub async fn create_room(
        &self,
        name: String,
        owner_id: Uuid,
    ) -> Result<(Room, bool), AppError> {
        let room_name = validate_room_name(&name)?;
        let room_key = normalize_room_key(&room_name);
        let mut name_index = self.name_index.write().await;

        if let Some(room_id) = name_index.get(&room_key)
            && let Some(room) = self.inner.read().await.get(room_id).cloned()
        {
            return Ok((room, false));
        }

        let room = Room {
            id: Uuid::new_v4().to_string(),
            name: room_name,
            owner_id,
            created_at: Utc::now(),
        };
        name_index.insert(room_key, room.id.clone());
        self.inner
            .write()
            .await
            .insert(room.id.clone(), room.clone());
        Ok((room, true))
    }

    pub async fn list_rooms(&self) -> Vec<Room> {
        let mut rooms = self
            .inner
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        rooms.sort_by_key(|room| std::cmp::Reverse(room.created_at));
        rooms
    }

    pub async fn find_by_id(&self, id: &str) -> Option<Room> {
        self.inner.read().await.get(id).cloned()
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateRoomRequest {
    name: String,
}

pub async fn list(
    State(state): State<AppState>,
    auth::CurrentUser(_user): auth::CurrentUser,
) -> Json<Vec<Room>> {
    Json(state.rooms.list_rooms().await)
}

pub async fn create(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Json(request): Json<CreateRoomRequest>,
) -> Result<(StatusCode, Json<Room>), AppError> {
    let (room, created) = state.rooms.create_room(request.name, user.id).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(room)))
}

fn validate_room_name(name: &str) -> Result<String, AppError> {
    let trimmed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let valid = !trimmed.is_empty()
        && trimmed.len() <= 80
        && trimmed.chars().all(|character| !character.is_control());
    if valid {
        Ok(trimmed.to_owned())
    } else {
        Err(AppError::BadRequest(
            "room name must be 1-80 visible characters".to_owned(),
        ))
    }
}

fn normalize_room_key(name: &str) -> String {
    name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{InMemoryRoomStore, normalize_room_key, validate_room_name};
    use uuid::Uuid;

    #[test]
    fn validates_room_names() {
        assert_eq!(validate_room_name(" Demo   room ").unwrap(), "Demo room");
        assert!(validate_room_name("").is_err());
        assert!(validate_room_name("\n").is_err());
        assert!(validate_room_name(&"a".repeat(81)).is_err());
    }

    #[test]
    fn normalizes_room_keys_case_insensitively() {
        assert_eq!(normalize_room_key("Nhóm Demo"), "nhóm demo");
    }

    #[tokio::test]
    async fn creating_same_room_name_returns_existing_room() {
        let store = InMemoryRoomStore::default();
        let owner_id = Uuid::new_v4();
        let (first_room, first_created) = store
            .create_room("Demo room".to_owned(), owner_id)
            .await
            .unwrap();
        let (second_room, second_created) = store
            .create_room(" demo   ROOM ".to_owned(), Uuid::new_v4())
            .await
            .unwrap();

        assert!(first_created);
        assert!(!second_created);
        assert_eq!(first_room.id, second_room.id);
        assert_eq!(store.list_rooms().await.len(), 1);
    }
}
