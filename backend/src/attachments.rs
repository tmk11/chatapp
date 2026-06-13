use crate::{auth, error::AppError, state::AppState};
use async_trait::async_trait;
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

pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_TOTAL_ATTACHMENT_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
struct StoredAttachment {
    owner_id: Uuid,
    /// Set once the attachment is bound to a message; prevents reuse.
    used: bool,
    content_type: &'static str,
    bytes: Bytes,
}

#[derive(Default)]
struct AttachmentMap {
    attachments: HashMap<Uuid, StoredAttachment>,
    total_bytes: usize,
}

#[async_trait]
pub trait AttachmentStore: Send + Sync {
    /// Validates and stores raw image/audio bytes. The content type is derived
    /// from the file's magic bytes, never from client-supplied headers.
    async fn store(&self, owner_id: Uuid, bytes: Bytes) -> Result<(Uuid, String), AppError>;

    /// Marks the attachment as used by a message. Fails if it does not belong
    /// to `owner_id` or was already used in another message.
    async fn mark_used(&self, id: Uuid, owner_id: Uuid) -> Result<(), AppError>;

    /// True if `user_id` is the uploader of the attachment.
    async fn is_owner(&self, id: Uuid, user_id: Uuid) -> Result<bool, AppError>;

    /// Returns the raw bytes; access control is enforced by the caller.
    async fn bytes(&self, id: Uuid) -> Result<(String, Bytes), AppError>;

    async fn remove(&self, id: Uuid) -> Result<(), AppError>;
}

#[derive(Clone, Default)]
pub struct InMemoryAttachmentStore {
    inner: Arc<RwLock<AttachmentMap>>,
}

#[async_trait]
impl AttachmentStore for InMemoryAttachmentStore {
    async fn store(&self, owner_id: Uuid, bytes: Bytes) -> Result<(Uuid, String), AppError> {
        let content_type = validate_media(&bytes)?;

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
                used: false,
                content_type,
                bytes,
            },
        );
        Ok((id, content_type.to_owned()))
    }

    async fn mark_used(&self, id: Uuid, owner_id: Uuid) -> Result<(), AppError> {
        let mut inner = self.inner.write().await;
        let attachment = inner.attachments.get_mut(&id).ok_or(AppError::NotFound)?;
        if attachment.owner_id != owner_id {
            return Err(AppError::NotFound);
        }
        if attachment.used {
            return Err(AppError::Conflict);
        }
        attachment.used = true;
        Ok(())
    }

    async fn is_owner(&self, id: Uuid, user_id: Uuid) -> Result<bool, AppError> {
        let inner = self.inner.read().await;
        Ok(inner
            .attachments
            .get(&id)
            .is_some_and(|attachment| attachment.owner_id == user_id))
    }

    async fn bytes(&self, id: Uuid) -> Result<(String, Bytes), AppError> {
        let inner = self.inner.read().await;
        let attachment = inner.attachments.get(&id).ok_or(AppError::NotFound)?;
        Ok((attachment.content_type.to_owned(), attachment.bytes.clone()))
    }

    async fn remove(&self, id: Uuid) -> Result<(), AppError> {
        let mut inner = self.inner.write().await;
        if let Some(attachment) = inner.attachments.remove(&id) {
            inner.total_bytes -= attachment.bytes.len();
        }
        Ok(())
    }
}

/// Shared validation: enforces the size limit and derives the content type
/// from magic bytes (images and common audio containers), never from headers.
pub fn validate_media(bytes: &Bytes) -> Result<&'static str, AppError> {
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(AppError::PayloadTooLarge);
    }
    sniff_media_type(bytes).ok_or(AppError::BadRequest(
        "unsupported file type (allowed: png, jpeg, gif, webp images; webm, ogg, mp4, mpeg, wav audio)"
            .to_owned(),
    ))
}

fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" {
        Some("audio/wav")
    } else if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        // EBML container — WebM/Matroska, as produced by MediaRecorder.
        Some("audio/webm")
    } else if bytes.starts_with(b"OggS") {
        Some("audio/ogg")
    } else if bytes.starts_with(b"ID3")
        || bytes.starts_with(&[0xFF, 0xFB])
        || bytes.starts_with(&[0xFF, 0xF3])
    {
        Some("audio/mpeg")
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        // ISO base media (mp4/m4a) — MediaRecorder audio/mp4.
        Some("audio/mp4")
    } else {
        None
    }
}

#[derive(Serialize)]
pub struct UploadResponse {
    id: Uuid,
    content_type: String,
    size: usize,
}

/// POST /attachments — authenticated; accepts raw image or audio bytes (max
/// 10 MiB) and returns an attachment id to reference in a message. The
/// attachment is private to its uploader until it is sent.
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

/// GET /attachments/{id} — authenticated; streams the file to its uploader, to
/// any member of a conversation it was sent in, or if it is a profile avatar.
pub async fn download(
    State(state): State<AppState>,
    auth::CurrentUser(user): auth::CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    let allowed = state.attachments.is_owner(id, user.id).await?
        || state.chat.attachment_visible(id, user.id).await?
        || state.users.avatar_in_use(id).await;
    if !allowed {
        return Err(AppError::NotFound);
    }
    let (content_type, bytes) = state.attachments.bytes(id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000".to_owned(),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::{AttachmentStore, InMemoryAttachmentStore, sniff_media_type};
    use axum::body::Bytes;
    use uuid::Uuid;

    fn png_bytes() -> Bytes {
        Bytes::from_static(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0])
    }

    #[test]
    fn sniffs_supported_media_types() {
        assert_eq!(sniff_media_type(&png_bytes()), Some("image/png"));
        assert_eq!(
            sniff_media_type(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_media_type(b"GIF89a..."), Some("image/gif"));
        assert_eq!(
            sniff_media_type(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(
            sniff_media_type(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0]),
            Some("audio/webm")
        );
        assert_eq!(sniff_media_type(b"OggS\x00\x02"), Some("audio/ogg"));
        assert_eq!(
            sniff_media_type(b"\x00\x00\x00\x20ftypM4A "),
            Some("audio/mp4")
        );
        assert_eq!(sniff_media_type(b"<svg onload=alert(1)>"), None);
        assert_eq!(sniff_media_type(b"plain text"), None);
    }

    #[tokio::test]
    async fn store_mark_used_and_bytes_flow() {
        let store = InMemoryAttachmentStore::default();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let (id, content_type) = store.store(alice, png_bytes()).await.unwrap();
        assert_eq!(content_type, "image/png");
        assert!(store.is_owner(id, alice).await.unwrap());
        assert!(!store.is_owner(id, bob).await.unwrap());

        // Only the owner can mark used, and only once.
        assert!(store.mark_used(id, bob).await.is_err());
        store.mark_used(id, alice).await.unwrap();
        assert!(store.mark_used(id, alice).await.is_err());

        assert!(store.bytes(id).await.is_ok());
        store.remove(id).await.unwrap();
        assert!(store.bytes(id).await.is_err());
    }

    #[tokio::test]
    async fn rejects_unsupported_bytes() {
        let store = InMemoryAttachmentStore::default();
        let owner = Uuid::new_v4();
        assert!(
            store
                .store(owner, Bytes::from_static(b"not media"))
                .await
                .is_err()
        );
    }
}
