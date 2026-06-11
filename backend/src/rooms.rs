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
}

impl InMemoryRoomStore {
    pub async fn create_room(&self, name: String, owner_id: Uuid) -> Result<Room, AppError> {
        let room = Room {
            id: Uuid::new_v4().to_string(),
            name: validate_room_name(&name)?,
            owner_id,
            created_at: Utc::now(),
        };
        self.inner
            .write()
            .await
            .insert(room.id.clone(), room.clone());
        Ok(room)
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
    let room = state.rooms.create_room(request.name, user.id).await?;
    Ok((StatusCode::CREATED, Json(room)))
}

fn validate_room_name(name: &str) -> Result<String, AppError> {
    let trimmed = name.trim();
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

#[cfg(test)]
mod tests {
    use super::validate_room_name;

    #[test]
    fn validates_room_names() {
        assert_eq!(validate_room_name(" Demo room ").unwrap(), "Demo room");
        assert!(validate_room_name("").is_err());
        assert!(validate_room_name("\n").is_err());
        assert!(validate_room_name(&"a".repeat(81)).is_err());
    }
}
