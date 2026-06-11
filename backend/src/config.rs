use anyhow::{Context, bail};
use std::{env, net::SocketAddr};

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub jwt_secret: String,
    pub jwt_ttl_seconds: i64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let host = env::var("APP_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
        let port = env::var("APP_PORT")
            .unwrap_or_else(|_| "8080".to_owned())
            .parse::<u16>()
            .context("APP_PORT must be a valid TCP port")?;
        let jwt_secret = env::var("JWT_SECRET")
            .unwrap_or_else(|_| "development-only-secret-change-before-production".to_owned());
        if jwt_secret.len() < 32 {
            bail!("JWT_SECRET must be at least 32 characters");
        }
        let jwt_ttl_seconds = env::var("JWT_TTL_SECONDS")
            .unwrap_or_else(|_| "3600".to_owned())
            .parse::<i64>()
            .context("JWT_TTL_SECONDS must be an integer")?;

        Ok(Self {
            host,
            port,
            jwt_secret,
            jwt_ttl_seconds,
        })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        format!("{}:{}", self.host, self.port)
            .parse()
            .expect("validated host and port must form a socket address")
    }
}
