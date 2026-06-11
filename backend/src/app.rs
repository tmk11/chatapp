use crate::{attachments, auth, config::Config, friends, messages, security, state::AppState, ws};
use axum::{
    Json, Router,
    routing::{get, post},
};
use serde::Serialize;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    services::ServeDir,
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

pub fn build_router(config: Config) -> Router {
    let frontend_dir = config.frontend_dir.clone();
    let state = AppState::new(config);
    let (content_type_name, content_type_value) = security::content_type_options_header();
    let (frame_name, frame_value) = security::frame_options_header();

    let api = Router::new()
        .route("/health", get(health))
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/friends", get(friends::list_friends))
        .route(
            "/friends/requests",
            get(friends::list_requests).post(friends::send_request),
        )
        .route("/friends/requests/{id}", post(friends::respond))
        .route(
            "/messages/{id}",
            get(messages::history).delete(messages::delete),
        )
        .route("/me", get(me))
        .route("/ws", get(ws::handler))
        .layer(RequestBodyLimitLayer::new(security::MAX_REQUEST_BODY_BYTES));

    // Image uploads need a larger body limit than the JSON API routes.
    let media = Router::new()
        .route("/attachments", post(attachments::upload))
        .route("/attachments/{id}", get(attachments::download))
        .layer(RequestBodyLimitLayer::new(
            attachments::MAX_ATTACHMENT_BYTES + 1024,
        ));

    Router::new()
        .merge(api)
        .merge(media)
        .fallback_service(ServeDir::new(frontend_dir))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(SetResponseHeaderLayer::overriding(
            content_type_name,
            content_type_value,
        ))
        .layer(SetResponseHeaderLayer::overriding(frame_name, frame_value))
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
