use crate::{
    auth::AuthService, config::Config, friends::InMemoryFriendStore,
    messages::InMemoryMessageStore, users::InMemoryUserStore, ws::UserHub,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub users: Arc<InMemoryUserStore>,
    pub friends: Arc<InMemoryFriendStore>,
    pub messages: Arc<InMemoryMessageStore>,
    pub user_hub: UserHub,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let users = Arc::new(InMemoryUserStore::default());
        let auth = Arc::new(AuthService::new(config, users.clone()));
        Self {
            auth,
            users,
            friends: Arc::new(InMemoryFriendStore::default()),
            messages: Arc::new(InMemoryMessageStore::default()),
            user_hub: UserHub::default(),
        }
    }
}
