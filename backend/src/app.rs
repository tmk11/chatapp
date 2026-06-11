use crate::{auth, config::Config, security, state::AppState, ws};
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde::Serialize;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

pub fn build_router(config: Config) -> Router {
    let state = AppState::new(config);
    let (content_type_name, content_type_value) = security::content_type_options_header();
    let (frame_name, frame_value) = security::frame_options_header();

    Router::new()
        .route("/health", get(health))
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/me", get(me))
        .route("/ws", get(ws::handler))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(SetResponseHeaderLayer::overriding(
            content_type_name,
            content_type_value,
        ))
        .layer(SetResponseHeaderLayer::overriding(frame_name, frame_value))
        .layer(RequestBodyLimitLayer::new(security::MAX_REQUEST_BODY_BYTES))
        .layer(CorsLayer::new().allow_origin(Any))
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn me(auth::CurrentUser(user): auth::CurrentUser) -> Json<crate::users::User> {
    Json(user)
}
