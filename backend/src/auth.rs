use crate::{
    config::Config,
    error::AppError,
    users::{User, UserStore},
};
use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json,
    extract::{FromRef, FromRequestParts},
    http::{StatusCode, request::Parts},
};
use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct AuthService {
    config: Config,
    users: Arc<dyn UserStore>,
}

impl AuthService {
    pub fn new(config: Config, users: Arc<dyn UserStore>) -> Self {
        Self { config, users }
    }

    pub async fn register(&self, request: RegisterRequest) -> Result<AuthResponse, AppError> {
        validate_password(&request.password)?;
        let password_hash = hash_password(&request.password)?;
        let user = self
            .users
            .create_user(request.phone, request.display_name, password_hash)
            .await?;
        let token = self.issue_token(user.id)?;
        Ok(AuthResponse { token, user })
    }

    pub async fn login(&self, request: LoginRequest) -> Result<AuthResponse, AppError> {
        let stored = self
            .users
            .find_by_phone(&request.phone)
            .await
            .ok_or(AppError::Unauthorized)?;
        verify_password(&request.password, &stored.password_hash)?;
        let token = self.issue_token(stored.user.id)?;
        Ok(AuthResponse {
            token,
            user: stored.user,
        })
    }

    pub fn validate_token(&self, token: &str) -> Result<Claims, AppError> {
        decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.config.jwt_secret.as_bytes()),
            &Validation::default(),
        )
        .map(|data| data.claims)
        .map_err(|_| AppError::Unauthorized)
    }

    fn issue_token(&self, user_id: Uuid) -> Result<String, AppError> {
        let now = Utc::now();
        let expires_at = now + Duration::seconds(self.config.jwt_ttl_seconds);
        let claims = Claims {
            sub: user_id,
            iat: now.timestamp() as usize,
            exp: expires_at.timestamp() as usize,
        };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.config.jwt_secret.as_bytes()),
        )
        .map_err(|_| AppError::Internal)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub iat: usize,
    pub exp: usize,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub phone: String,
    pub display_name: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub phone: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: User,
}

pub async fn register(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    Json(request): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    Ok(Json(state.auth.register(request).await?))
}

pub async fn login(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    Ok(Json(state.auth.login(request).await?))
}

pub struct CurrentUser(pub User);

impl<S> FromRequestParts<S> for CurrentUser
where
    crate::state::AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = crate::state::AppState::from_ref(state);
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token"))?;
        let claims = app_state
            .auth
            .validate_token(token)
            .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid bearer token"))?;
        let user = app_state
            .users
            .find_by_id(claims.sub)
            .await
            .ok_or((StatusCode::UNAUTHORIZED, "unknown user"))?;
        Ok(Self(user))
    }
}

fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AppError::Internal)
}

fn verify_password(password: &str, password_hash: &str) -> Result<(), AppError> {
    let parsed_hash = PasswordHash::new(password_hash).map_err(|_| AppError::Unauthorized)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| AppError::Unauthorized)
}

fn validate_password(password: &str) -> Result<(), AppError> {
    if password.len() < 12 {
        return Err(AppError::BadRequest(
            "password must contain at least 12 characters".to_owned(),
        ));
    }
    Ok(())
}
