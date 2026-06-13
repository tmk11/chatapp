# Secure Chat App — Goals, Context, Rules, and Roadmap

This file is the handoff document for future AI agents and engineers. Read it before making changes.

## Product goal

Build a WhatsApp-like chat app with strong security, reliable real-time messaging, and an architecture that can scale toward 1 million users.

Core user-facing capabilities:

1. Account registration and login by phone number.
2. Friend requests by phone number; only friends can start chats / be added to groups. There are no public rooms — chats are contact-based (decision 2026-06).
3. Unified conversations: 1:1 direct chats (created on demand between friends) and group chats (title, owner, add/remove members), real-time over one WebSocket.
4. Stored conversation history (durable in Postgres when DATABASE_URL is set).
5. Message deletion: delete for me (per-user hide) and delete for everyone (sender-only tombstone), WhatsApp-style.
6. Image and voice messages (development baseline; production needs encrypted object storage, malware scanning, expiring URLs).
7. Read receipts via per-member read cursors (`conversation_members.last_read_at`); `read_by` set per message. Delivered-state was dropped in the conversation refactor.
8. Typing indicators and online/offline presence with last-seen (implemented).
9. Conversation list (directs + groups) with unread counts and last-message previews (implemented).
10. Replies, emoji reactions (fixed set), message pinning, and in-conversation search (implemented).
11. Profile avatars (implemented).
12. Offline message sync (history reload works; per-device cursors still pending).
13. Push notifications.
14. Contact discovery with privacy controls.
15. End-to-end encryption for message content.
16. Abuse prevention, rate limiting, reporting, and moderation workflows.

## Current implementation

The repository currently contains a Rust backend scaffold in `backend/` plus a static development frontend in `frontend/`:

- Axum HTTP server and Tokio async runtime.
- JWT authentication.
- Argon2 password hashing.
- Storage behind per-domain traits (`UserStore`, `FriendStore`, `ChatStore`, `AttachmentStore`) with two implementations each: durable Postgres stores (`pg.rs`, selected when DATABASE_URL is set; SQLx migrations in `backend/migrations/` run automatically at startup, `0002_conversations.sql` migrates legacy 1:1 data) and in-memory development stores (the default).
- Friend store (`friends.rs`): friend requests by phone number, accept/decline, mutual-request auto-accept, friendship checks. Friendships are unordered user-id pairs. `GET /friends` returns the friend list with presence (used to start chats / pick group members).
- Chat store (`chat.rs`): the unified conversation model. Conversations have members with roles (owner/admin/member) and a `last_read_at` cursor; direct conversations are keyed by the unordered pair and created on demand, groups have a title and owner. Messages carry kind (text/image/voice), body, attachment, `duration_ms` (voice), reply (validated to the same conversation, preview embedded at read time), reactions (fixed set, participants only, cleared on tombstone), `pinned`, and per-message `read_by` (members whose cursor ≥ sent_at). Supports history, conversation-scoped case-insensitive text search, pins, per-user delete-for-me, sender-only delete-for-everyone (purges media), and membership management. Authorization mirrors between both impls.
- Attachment store (`attachments.rs`): raw image (PNG/JPEG/GIF/WebP) or audio (WebM/OGG/MP4/MPEG/WAV) upload, max 10 MiB, magic-byte content-type sniffing. Attachments are owner-private until `mark_used` (single-use); `download` is allowed for the owner, any member of the conversation referencing it (`ChatStore::attachment_visible`), or if it is a profile avatar (`UserStore::avatar_in_use`).
- WebSocket fanout with presence (`ws.rs`): one connection per session, JSON frames tagged with `type`. Inbound: `message` (kind text/image/voice, conversation_id), `read`, `typing`, `reaction`. Outbound (fanned out to conversation members): `message`, `message_deleted`, `message_pinned`, `read` (conversation_id + user_id + at), `typing`, `presence`, `reaction`, `conversation_updated`, `error`. Connection counting drives presence to friends; `users.last_seen_at` persisted on last disconnect.
- Receipts: members advance their cursor via the `read` frame (or implicitly when sending); senders see `read` events and compute per-message read state from `read_by` (direct → ✓✓; group → seen count).
- Endpoints: health, auth, profile (`/me`, `/me/avatar`), friends, `/conversations*` (list, direct, group, members, messages, search, pins), `/messages/{id}` (delete) and `/messages/{id}/pin`, attachments, WebSocket. Attachment routes use a larger body limit (10 MiB + slack) than the 64 KiB JSON API limit.
- Static "Ripple" web frontend: light-first with automatic dark mode, brand gradient, colourful avatars (with profile-photo override), chat bubbles, mobile single-pane (`show-chat` on `#app-screen`). Supports conversation list (directs + groups), a new-chat/new-group modal, group info (add member / leave), per-conversation search bar and pinned bar, voice recording via MediaRecorder, image sending, replies, reactions, pin, and deletion. UI copy is Vietnamese.
- Backend static file serving from configurable `FRONTEND_DIR`, defaulting to the repository `frontend/` directory for local development.
- Security headers and request body size limits.

The current implementation is suitable for local development and architectural iteration. It is not yet production-ready.

## Target architecture for 1 million users

Use a horizontally scalable service architecture:

- API gateway or load balancer terminates TLS and routes to backend instances.
- Rust chat API instances remain stateless except for active WebSocket connections.
- Postgres stores users, devices, conversations, memberships, message metadata, and encrypted message payloads.
- Redis Cluster stores short-lived sessions, rate-limit counters, presence, WebSocket node routing, and idempotency keys.
- NATS, Kafka, or Redpanda handles durable message fanout between WebSocket nodes.
- Object storage stores encrypted media files.
- Push workers send APNs/FCM notifications without exposing plaintext message content.
- Observability stack includes OpenTelemetry traces, Prometheus metrics, structured logs, and SLO dashboards.

Important scaling rules:

- Never rely on in-memory state for data that must survive deploys or node failures.
- Partition high-volume data by conversation ID or user ID.
- Use backpressure everywhere: WebSocket queues, fanout consumers, database pools, and media uploads.
- Design all write APIs to be idempotent.
- Keep hot paths free of blocking calls.
- Load test connection counts, message fanout, reconnect storms, and push notification bursts before production.

## Security goals

Security is a primary product requirement, not an afterthought.

Required controls before production:

- TLS everywhere, including internal service-to-service traffic where possible.
- End-to-end encryption using audited protocols such as Signal Double Ratchet or MLS. Do not invent custom cryptography.
- Per-device identity keys, signed prekeys, and one-time prekeys.
- Encrypted message storage; the server should not need plaintext message bodies.
- Secure media upload flow with encrypted files, malware scanning, expiring URLs, and strict content limits.
- Strong password policy if passwords remain supported; prefer passkeys and device-bound sessions long term.
- Short-lived access tokens plus refresh-token rotation and revocation.
- Rate limiting by IP, account, phone number, device, and endpoint.
- Audit logs for account, device, admin, and security-sensitive events.
- Secret management through a vault or cloud secret manager. Do not commit real secrets.
- Dependency scanning, static analysis, container scanning, and regular patching.
- Privacy-safe logs: never log tokens, passwords, private keys, plaintext messages, or contact lists.

## Code rules for future agents

Follow these rules unless a later human instruction explicitly overrides them:

1. Use Rust for backend services.
2. Prefer simple, explicit, testable code over clever abstractions.
3. Never add `unwrap()` or `expect()` in request-handling paths. Convert errors to safe API responses.
4. Never put `try/catch`-style wrappers around imports in languages that support them.
5. Never log secrets, credentials, tokens, private keys, or plaintext message bodies.
6. Treat browser token storage in the current frontend as development-only until production session management, refresh-token rotation, revocation, and stronger client-side security controls are implemented.
7. Keep authentication, authorization, storage, and transport concerns separated by module boundaries.
8. Add tests for security-sensitive logic, auth flows, message validation, and storage adapters.
9. New public APIs must document authentication, authorization, validation, idempotency, and rate limits.
10. Any persistent schema change must include migrations and rollback notes.
11. Any new background worker must define retry, dead-letter, idempotency, and observability behavior.
12. Any feature that changes the runnable web app should include a screenshot when feasible.
13. Keep this file updated whenever goals, architecture, coding rules, or roadmap items change.

## Immediate roadmap

Complete these tasks next:

1. Add refresh tokens, device sessions, logout, secure browser session handling, and token revocation.
2. Add Redis-backed distributed rate limits.
3. Add unfriend/block flows and per-contact privacy controls.
4. Encrypt stored message bodies (storage is durable in Postgres already; encryption at rest and E2EE are still pending).
5. Move image attachment bytes from Postgres to encrypted object storage with malware scanning, expiring URLs, and thumbnail generation; extend to video, audio, and documents.
6. Add message IDs generated by clients for idempotency.
7. Add distributed fanout with NATS, Kafka, or Redpanda (presence and WebSocket routing are single-node today).
8. Add offline sync pagination and per-device cursors (history currently loads whole conversations).
9. Add end-to-end encryption protocol support.
10. Add OpenTelemetry tracing and Prometheus metrics.
11. Add CI with formatting, clippy, tests (including the TEST_DATABASE_URL-gated Postgres integration tests), audit, and container scan.
12. Add production frontend build pipeline, CSP, secure cookie/session strategy, and frontend integration tests.
13. Add load-test scenarios for 10k, 100k, and 1M-user growth phases.

## Definition of done for backend changes

A backend change is done only when:

- `cargo fmt --check` passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes or the limitation is documented.
- `cargo test --all` passes. Postgres integration tests in `pg.rs` run only when `TEST_DATABASE_URL` is set; run them against a scratch database when storage code changes.
- New behavior has tests or a written reason tests are deferred.
- Security and privacy impacts are considered, including browser token handling for frontend changes.
- This context file is updated if architecture, roadmap, or rules changed.
