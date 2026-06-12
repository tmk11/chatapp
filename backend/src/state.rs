use crate::{
    attachments::{AttachmentStore, InMemoryAttachmentStore},
    auth::AuthService,
    config::Config,
    friends::{FriendStore, InMemoryFriendStore},
    messages::{InMemoryMessageStore, MessageStore},
    pg::{PgAttachmentStore, PgFriendStore, PgMessageStore, PgUserStore},
    users::{InMemoryUserStore, UserStore},
    ws::UserHub,
};
use anyhow::Context;
use std::sync::Arc;
use tracing::info;

type Stores = (
    Arc<dyn UserStore>,
    Arc<dyn FriendStore>,
    Arc<dyn MessageStore>,
    Arc<dyn AttachmentStore>,
);

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<AuthService>,
    pub users: Arc<dyn UserStore>,
    pub friends: Arc<dyn FriendStore>,
    pub messages: Arc<dyn MessageStore>,
    pub attachments: Arc<dyn AttachmentStore>,
    pub user_hub: UserHub,
}

impl AppState {
    /// Builds the application state. With DATABASE_URL set this connects to
    /// Postgres and runs pending migrations; otherwise it falls back to the
    /// development in-memory stores.
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let (users, friends, messages, attachments): Stores = match &config.database_url {
            Some(database_url) => {
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(16)
                    .connect(database_url)
                    .await
                    .context("failed to connect to DATABASE_URL")?;
                sqlx::migrate!("./migrations")
                    .run(&pool)
                    .await
                    .context("failed to run database migrations")?;
                info!("using postgres storage");
                (
                    Arc::new(PgUserStore::new(pool.clone())),
                    Arc::new(PgFriendStore::new(pool.clone())),
                    Arc::new(PgMessageStore::new(pool.clone())),
                    Arc::new(PgAttachmentStore::new(pool)),
                )
            }
            None => {
                info!("DATABASE_URL not set; using in-memory development storage");
                (
                    Arc::new(InMemoryUserStore::default()),
                    Arc::new(InMemoryFriendStore::default()),
                    Arc::new(InMemoryMessageStore::default()),
                    Arc::new(InMemoryAttachmentStore::default()),
                )
            }
        };
        let auth = Arc::new(AuthService::new(config, users.clone()));
        Ok(Self {
            auth,
            users,
            friends,
            messages,
            attachments,
            user_hub: UserHub::default(),
        })
    }
}
