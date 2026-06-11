use axum::http::{HeaderName, HeaderValue};

pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;

pub fn content_type_options_header() -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    )
}

pub fn frame_options_header() -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    )
}
