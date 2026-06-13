use crate::{
    auth,
    error::AppError,
    state::AppState,
    users::{User, normalize_phone},
};
use async_trait::async_trait;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct FriendRequest {
    pub id: Uuid,
    pub from_user_id: Uuid,
    pub to_user_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    Pending(Uuid),
    Accepted,
}

#[async_trait]
pub trait FriendStore: Send + Sync {
    async fn are_friends(&self, a: Uuid, b: Uuid) -> Result<bool, AppError>;
    async fn send_request(&self, from: Uuid, to: Uuid) -> Result<RequestOutcome, AppError>;
    async fn incoming_requests(&self, user_id: Uuid) -> Result<Vec<FriendRequest>, AppError>;
    async fn respond(
        &self,
        request_id: Uuid,
        user_id: Uuid,
        accept: bool,
    ) -> Result<FriendRequest, AppError>;
    async fn friends_of(&self, user_id: Uuid) -> Result<Vec<Uuid>, AppError>;
}

pub fn friendship_key(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a <= b { (a, b) } else { (b, a) }
}

#[derive(Clone, Default)]
pub struct InMemoryFriendStore {
    requests: Arc<RwLock<HashMap<Uuid, FriendRequest>>>,
    friendships: Arc<RwLock<HashSet<(Uuid, Uuid)>>>,
}

#[async_trait]
impl FriendStore for InMemoryFriendStore {
    async fn are_friends(&self, a: Uuid, b: Uuid) -> Result<bool, AppError> {
        Ok(self
            .friendships
            .read()
            .await
            .contains(&friendship_key(a, b)))
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

        let mut requests = self.requests.write().await;
        let existing_outgoing = requests
            .values()
            .find(|request| request.from_user_id == from && request.to_user_id == to)
            .map(|request| request.id);
        if let Some(request_id) = existing_outgoing {
            return Ok(RequestOutcome::Pending(request_id));
        }

        let mutual_incoming = requests
            .values()
            .find(|request| request.from_user_id == to && request.to_user_id == from)
            .map(|request| request.id);
        if let Some(request_id) = mutual_incoming {
            requests.remove(&request_id);
            self.friendships
                .write()
                .await
                .insert(friendship_key(from, to));
            return Ok(RequestOutcome::Accepted);
        }

        let request = FriendRequest {
            id: Uuid::new_v4(),
            from_user_id: from,
            to_user_id: to,
            created_at: Utc::now(),
        };
        let request_id = request.id;
        requests.insert(request_id, request);
        Ok(RequestOutcome::Pending(request_id))
    }

    async fn incoming_requests(&self, user_id: Uuid) -> Result<Vec<FriendRequest>, AppError> {
        let mut incoming = self
            .requests
            .read()
            .await
            .values()
            .filter(|request| request.to_user_id == user_id)
            .cloned()
            .collect::<Vec<_>>();
        incoming.sort_by_key(|request| request.created_at);
        Ok(incoming)
    }

    async fn respond(
        &self,
        request_id: Uuid,
        user_id: Uuid,
        accept: bool,
    ) -> Result<FriendRequest, AppError> {
        let mut requests = self.requests.write().await;
        let request = requests
            .get(&request_id)
            .cloned()
            .ok_or(AppError::NotFound)?;
        if request.to_user_id != user_id {
            return Err(AppError::NotFound);
        }
        requests.remove(&request_id);
        if accept {
            self.friendships
                .write()
                .await
                .insert(friendship_key(request.from_user_id, request.to_user_id));
        }
        Ok(request)
    }

    async fn friends_of(&self, user_id: Uuid) -> Result<Vec<Uuid>, AppError> {
        Ok(self
            .friendships
            .read()
            .await
            .iter()
            .filter_map(|(a, b)| {
                if *a == user_id {
                    Some(*b)
                } else if *b == user_id {
                    Some(*a)
                } else {
                    None
                }
            })
            .collect())
    }
}

#[derive(Debug, Deserialize)]
pub struct SendRequestBody {
    phone: String,
}

#[derive(Debug, Serialize)]
pub struct SendRequestResponse {
    status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct IncomingRequestView {
    id: Uuid,
    from: User,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct RespondBody {
    accept: bool,
}

#[derive(Debug, Serialize)]
pub struct RespondResponse {
    status: &'static str,
}

/// A friend entry for the "new chat" / "add to group" pickers.
#[derive(Debug, Serialize)]
pub struct FriendView {
    #[serde(flatten)]
    pub user: User,
    pub online: bool,
}

/// POST /friends/requests — authenticated; sends a friend request to the user
/// owning the given phone number. If that user already sent us a request, the
/// two requests are merged and the friendship is created immediately.
pub async fn send_request(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Json(body): Json<SendRequestBody>,
) -> Result<(StatusCode, Json<SendRequestResponse>), AppError> {
    let normalized_phone = normalize_phone(&body.phone)?;
    let target = state
        .users
        .find_by_phone(&normalized_phone)
        .await
        .ok_or(AppError::NotFound)?;
    let outcome = state.friends.send_request(user.id, target.user.id).await?;
    let (status_code, status) = match outcome {
        RequestOutcome::Pending(_) => (StatusCode::CREATED, "pending"),
        RequestOutcome::Accepted => (StatusCode::OK, "accepted"),
    };
    Ok((status_code, Json(SendRequestResponse { status })))
}

/// GET /friends/requests — authenticated; lists pending requests sent to the
/// current user, including sender profile information.
pub async fn list_requests(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
) -> Result<Json<Vec<IncomingRequestView>>, AppError> {
    let incoming = state.friends.incoming_requests(user.id).await?;
    let mut views = Vec::with_capacity(incoming.len());
    for request in incoming {
        if let Some(sender) = state.users.find_by_id(request.from_user_id).await {
            views.push(IncomingRequestView {
                id: request.id,
                from: sender,
                created_at: request.created_at,
            });
        }
    }
    Ok(Json(views))
}

/// POST /friends/requests/{id} — authenticated; accepts or declines a pending
/// request addressed to the current user.
pub async fn respond(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(request_id): Path<Uuid>,
    Json(body): Json<RespondBody>,
) -> Result<Json<RespondResponse>, AppError> {
    state
        .friends
        .respond(request_id, user.id, body.accept)
        .await?;
    Ok(Json(RespondResponse {
        status: if body.accept { "accepted" } else { "declined" },
    }))
}

/// GET /friends — authenticated; the user's friends with presence, sorted by
/// name. Used to start new direct chats and to pick group members; the chat
/// list itself lives at `/conversations`.
pub async fn list_friends(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
) -> Result<Json<Vec<FriendView>>, AppError> {
    let friend_ids = state.friends.friends_of(user.id).await?;
    let mut friends = Vec::with_capacity(friend_ids.len());
    for friend_id in friend_ids {
        if let Some(friend) = state.users.find_by_id(friend_id).await {
            friends.push(FriendView {
                online: state.user_hub.is_online(friend.id).await,
                user: friend,
            });
        }
    }
    friends.sort_by(|a, b| a.user.display_name.cmp(&b.user.display_name));
    Ok(Json(friends))
}

#[cfg(test)]
mod tests {
    use super::{FriendStore, InMemoryFriendStore, RequestOutcome};
    use uuid::Uuid;

    #[tokio::test]
    async fn request_then_accept_creates_friendship() {
        let store = InMemoryFriendStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let outcome = store.send_request(alice, bob).await.unwrap();
        let request_id = match outcome {
            RequestOutcome::Pending(id) => id,
            other => panic!("expected pending request, got {other:?}"),
        };
        assert!(!store.are_friends(alice, bob).await.unwrap());

        store.respond(request_id, bob, true).await.unwrap();
        assert!(store.are_friends(alice, bob).await.unwrap());
        assert!(store.are_friends(bob, alice).await.unwrap());
        assert_eq!(store.friends_of(alice).await.unwrap(), vec![bob]);
    }

    #[tokio::test]
    async fn declining_request_does_not_create_friendship() {
        let store = InMemoryFriendStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let RequestOutcome::Pending(request_id) = store.send_request(alice, bob).await.unwrap()
        else {
            panic!("expected pending request");
        };
        store.respond(request_id, bob, false).await.unwrap();
        assert!(!store.are_friends(alice, bob).await.unwrap());
        assert!(store.incoming_requests(bob).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn mutual_requests_become_friendship() {
        let store = InMemoryFriendStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        store.send_request(alice, bob).await.unwrap();
        let outcome = store.send_request(bob, alice).await.unwrap();
        assert_eq!(outcome, RequestOutcome::Accepted);
        assert!(store.are_friends(alice, bob).await.unwrap());
        assert!(store.incoming_requests(alice).await.unwrap().is_empty());
        assert!(store.incoming_requests(bob).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn cannot_respond_to_someone_elses_request() {
        let store = InMemoryFriendStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let mallory = Uuid::new_v4();

        let RequestOutcome::Pending(request_id) = store.send_request(alice, bob).await.unwrap()
        else {
            panic!("expected pending request");
        };
        assert!(store.respond(request_id, mallory, true).await.is_err());
        assert!(!store.are_friends(alice, bob).await.unwrap());
    }

    #[tokio::test]
    async fn self_and_duplicate_requests_are_handled() {
        let store = InMemoryFriendStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        assert!(store.send_request(alice, alice).await.is_err());

        let RequestOutcome::Pending(first) = store.send_request(alice, bob).await.unwrap() else {
            panic!("expected pending request");
        };
        let RequestOutcome::Pending(second) = store.send_request(alice, bob).await.unwrap() else {
            panic!("expected pending request");
        };
        assert_eq!(first, second);
        assert_eq!(store.incoming_requests(bob).await.unwrap().len(), 1);
    }
}
