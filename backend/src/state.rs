use crate::{
    auth::AuthService, config::Config, rooms::InMemoryRoomStore, users::InMemoryUserStore,
    ws::RoomHub,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub users: Arc<InMemoryUserStore>,
    pub rooms: Arc<InMemoryRoomStore>,
    pub room_hub: RoomHub,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let users = Arc::new(InMemoryUserStore::default());
        let auth = Arc::new(AuthService::new(config, users.clone()));
        Self {
            auth,
            users,
            rooms: Arc::new(InMemoryRoomStore::default()),
            room_hub: RoomHub::default(),
        }
    }
}
