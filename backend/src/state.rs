use crate::{auth::AuthService, config::Config, users::InMemoryUserStore, ws::RoomHub};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub users: Arc<InMemoryUserStore>,
    pub rooms: RoomHub,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let users = Arc::new(InMemoryUserStore::default());
        let auth = Arc::new(AuthService::new(config, users.clone()));
        Self {
            auth,
            users,
            rooms: RoomHub::default(),
        }
    }
}
