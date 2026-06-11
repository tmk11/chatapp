use crate::{auth, error::AppError, state::AppState};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use uuid::Uuid;

pub const MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
const MAX_TOTAL_ATTACHMENT_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
struct StoredAttachment {
    owner_id: Uuid,
    /// Set once the attachment is bound to a message; grants read access to
    /// the message recipient and prevents reuse in another message.
    recipient_id: Option<Uuid>,
    content_type: &'static str,
    bytes: Bytes,
}

#[derive(Default)]
struct AttachmentMap {
    attachments: HashMap<Uuid, StoredAttachment>,
    total_bytes: usize,
}

#[derive(Clone, Default)]
pub struct InMemoryAttachmentStore {
    inner: Arc<RwLock<AttachmentMap>>,
}

impl InMemoryAttachmentStore {
    /// Validates and stores raw image bytes. The content type is derived from
    /// the file's magic bytes, never from client-supplied headers.
    pub async fn store(
        &self,
        owner_id: Uuid,
        bytes: Bytes,
    ) -> Result<(Uuid, &'static str), AppError> {
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(AppError::PayloadTooLarge);
        }
        let content_type = sniff_image_type(&bytes).ok_or(AppError::BadRequest(
            "only png, jpeg, gif, or webp images are supported".to_owned(),
        ))?;

        let mut inner = self.inner.write().await;
        if inner.total_bytes + bytes.len() > MAX_TOTAL_ATTACHMENT_BYTES {
            return Err(AppError::PayloadTooLarge);
        }
        let id = Uuid::new_v4();
        inner.total_bytes += bytes.len();
        inner.attachments.insert(
            id,
            StoredAttachment {
                owner_id,
                recipient_id: None,
                content_type,
                bytes,
            },
        );
        Ok((id, content_type))
    }

    /// Binds an uploaded attachment to a message from `owner_id` to
    /// `recipient_id`. Fails if the attachment does not belong to the sender
    /// or was already used in another message.
    pub async fn attach(
        &self,
        id: Uuid,
        owner_id: Uuid,
        recipient_id: Uuid,
    ) -> Result<(), AppError> {
        let mut inner = self.inner.write().await;
        let attachment = inner.attachments.get_mut(&id).ok_or(AppError::NotFound)?;
        if attachment.owner_id != owner_id {
            return Err(AppError::NotFound);
        }
        if attachment.recipient_id.is_some() {
            return Err(AppError::Conflict);
        }
        attachment.recipient_id = Some(recipient_id);
        Ok(())
    }

    /// Returns the image for the uploader or, once attached, the recipient.
    pub async fn fetch(&self, id: Uuid, user_id: Uuid) -> Result<(&'static str, Bytes), AppError> {
        let inner = self.inner.read().await;
        let attachment = inner.attachments.get(&id).ok_or(AppError::NotFound)?;
        let allowed = attachment.owner_id == user_id || attachment.recipient_id == Some(user_id);
        if !allowed {
            return Err(AppError::NotFound);
        }
        Ok((attachment.content_type, attachment.bytes.clone()))
    }

    pub async fn remove(&self, id: Uuid) {
        let mut inner = self.inner.write().await;
        if let Some(attachment) = inner.attachments.remove(&id) {
            inner.total_bytes -= attachment.bytes.len();
        }
    }
}

fn sniff_image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[derive(Serialize)]
pub struct UploadResponse {
    id: Uuid,
    content_type: &'static str,
    size: usize,
}

/// POST /attachments — authenticated; accepts raw image bytes (png, jpeg,
/// gif, or webp; max 5 MiB) and returns an attachment id to reference in an
/// image message. The attachment is only visible to its uploader until it is
/// sent to a friend.
pub async fn upload(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    bytes: Bytes,
) -> Result<(StatusCode, Json<UploadResponse>), AppError> {
    let size = bytes.len();
    let (id, content_type) = state.attachments.store(user.id, bytes).await?;
    Ok((
        StatusCode::CREATED,
        Json(UploadResponse {
            id,
            content_type,
            size,
        }),
    ))
}

/// GET /attachments/{id} — authenticated; streams the image to the uploader
/// or the recipient of the message it was sent with.
pub async fn download(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    let (content_type, bytes) = state.attachments.fetch(id, user.id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "private, max-age=31536000"),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::{InMemoryAttachmentStore, sniff_image_type};
    use axum::body::Bytes;
    use uuid::Uuid;

    fn png_bytes() -> Bytes {
        Bytes::from_static(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0])
    }

    #[test]
    fn sniffs_supported_image_types() {
        assert_eq!(sniff_image_type(&png_bytes()), Some("image/png"));
        assert_eq!(
            sniff_image_type(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_image_type(b"GIF89a..."), Some("image/gif"));
        assert_eq!(
            sniff_image_type(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_image_type(b"<svg onload=alert(1)>"), None);
        assert_eq!(sniff_image_type(b"plain text"), None);
    }

    #[tokio::test]
    async fn upload_attach_and_fetch_flow() {
        let store = InMemoryAttachmentStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let mallory = Uuid::new_v4();

        let (id, content_type) = store.store(alice, png_bytes()).await.unwrap();
        assert_eq!(content_type, "image/png");

        // Before attaching, only the uploader can fetch.
        assert!(store.fetch(id, alice).await.is_ok());
        assert!(store.fetch(id, bob).await.is_err());

        // Only the uploader can attach, and only once.
        assert!(store.attach(id, bob, alice).await.is_err());
        store.attach(id, alice, bob).await.unwrap();
        assert!(store.attach(id, alice, bob).await.is_err());

        assert!(store.fetch(id, bob).await.is_ok());
        assert!(store.fetch(id, mallory).await.is_err());

        store.remove(id).await;
        assert!(store.fetch(id, alice).await.is_err());
    }

    #[tokio::test]
    async fn rejects_unsupported_bytes() {
        let store = InMemoryAttachmentStore::default();
        let owner = Uuid::new_v4();
        assert!(
            store
                .store(owner, Bytes::from_static(b"not an image"))
                .await
                .is_err()
        );
    }
}
