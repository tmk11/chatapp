use crate::{
    attachments, auth, chat, config::Config, friends, security, state::AppState, users, ws,
};
use axum::{
    Json, Router,
    http::{HeaderValue, header},
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

pub async fn build_router(config: Config) -> anyhow::Result<Router> {
    let frontend_dir = config.frontend_dir.clone();
    let state = AppState::new(config).await?;
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
        .route("/conversations", get(chat::list_conversations))
        .route("/conversations/direct", post(chat::open_direct))
        .route("/conversations/group", post(chat::create_group))
        .route("/conversations/{id}/messages", get(chat::history))
        .route("/conversations/{id}/search", get(chat::search))
        .route("/conversations/{id}/pins", get(chat::pinned))
        .route("/conversations/{id}/members", post(chat::add_member))
        .route(
            "/conversations/{id}/members/{user_id}",
            axum::routing::delete(chat::remove_member),
        )
        .route("/messages/{id}", axum::routing::delete(chat::delete))
        .route("/messages/{id}/pin", post(chat::set_pin))
        .route("/me", get(me))
        .route("/me/avatar", axum::routing::put(users::set_avatar))
        .route("/ws", get(ws::handler))
        .layer(RequestBodyLimitLayer::new(security::MAX_REQUEST_BODY_BYTES));

    // Image uploads need a larger body limit than the JSON API routes.
    let media = Router::new()
        .route("/attachments", post(attachments::upload))
        .route("/attachments/{id}", get(attachments::download))
        .layer(RequestBodyLimitLayer::new(
            attachments::MAX_ATTACHMENT_BYTES + 1024,
        ));

    Ok(Router::new()
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
        // Force browsers to revalidate static frontend assets so a stale,
        // cached app.js is never served after a deploy. `if_not_present`
        // leaves the attachments handler's own long-lived Cache-Control intact.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .layer(CorsLayer::new().allow_origin(Any)))
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
