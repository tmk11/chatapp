# Secure Chat App — Goals, Context, Rules, and Roadmap

This file is the handoff document for future AI agents and engineers. Read it before making changes.

## Product goal

Build a WhatsApp-like chat app with strong security, reliable real-time messaging, and an architecture that can scale toward 1 million users.

Core user-facing capabilities:

1. Account registration and login by phone number.
2. Friend requests by phone number; only friends can message each other. There are no chat rooms — this was an explicit product decision (2026-06) replacing the earlier room feature.
3. 1:1 real-time text messaging (group chats may return later, but contact-based, not room-based).
4. Stored conversation history (durable in Postgres when DATABASE_URL is set).
5. Message deletion: delete for me (per-user hide) and delete for everyone (sender-only tombstone), WhatsApp-style.
6. Image messages (implemented as a development baseline; production needs encrypted object storage, malware scanning, and expiring URLs).
7. Message delivery/read receipts (implemented: client-acked delivery, conversation-level read).
8. Typing indicators and online/offline presence with last-seen (implemented).
9. Conversation list with unread counts and last-message previews (implemented).
10. Replies (quoted messages) and emoji reactions (implemented with a fixed emoji set).
11. Offline message sync (history reload works; per-device cursors still pending).
12. Other media attachments (video, audio, documents).
13. Push notifications.
14. Contact discovery with privacy controls.
15. End-to-end encryption for message content.
16. Abuse prevention, rate limiting, reporting, and moderation workflows.

## Current implementation

The repository currently contains a Rust backend scaffold in `backend/` plus a static development frontend in `frontend/`:

- Axum HTTP server and Tokio async runtime.
- JWT authentication.
- Argon2 password hashing.
- Storage behind per-domain traits (`UserStore`, `FriendStore`, `MessageStore`, `AttachmentStore`) with two implementations each: durable Postgres stores (`pg.rs`, selected when DATABASE_URL is set; SQLx migrations in `backend/migrations/` run automatically at startup) and in-memory development stores (the default).
- Friend store (`friends.rs`): friend requests by phone number, accept/decline, mutual-request auto-accept, friendship checks. Friendships are stored as unordered user-id pairs. `GET /friends` returns the conversation list: each friend with presence, unread count, and last visible message, sorted by latest activity.
- Message store (`messages.rs`): 1:1 conversations keyed by the unordered participant pair, text and image message kinds, replies (validated to the same conversation, previews embedded at read time), emoji reactions (fixed set 👍 ❤️ 😂 😮 😢 🙏, toggled, participants only, cleared on tombstone), delivery/read receipt timestamps, per-user delete-for-me hiding, and sender-only delete-for-everyone tombstones (which also purge image attachments).
- Attachment store (`attachments.rs`): raw image upload (PNG/JPEG/GIF/WebP, max 5 MiB per image) with magic-byte content-type sniffing, uploader-only access until the image is sent, single-use binding to one message, and authenticated download for the two participants only. The in-memory store caps total bytes at 256 MiB; the Postgres store keeps bytes in the `attachments` table (object storage is a roadmap item).
- Per-user WebSocket fanout with presence tracking (`ws.rs`): one connection per session, JSON frames tagged with `type` (`message`, `delivered`, `read`, `typing`, `reaction` inbound; plus `message_deleted`, `presence`, `error` outbound). Connection counting per user drives presence: friends receive `presence` events on first-connect/last-disconnect, and `users.last_seen_at` is persisted on disconnect. Sending requires friendship.
- Receipts: recipients ack delivery explicitly (`delivered` frame with message ids) and mark whole conversations read (`read` frame); senders receive `delivered`/`read` events. Read implies delivered.
- Basic health, auth, profile, friends, messages, attachments, and WebSocket endpoints. The attachments routes use a larger request body limit (5 MiB + slack) than the 64 KiB JSON API limit.
- Static dark-first web frontend with an auth-first flow: login/signup, add-friend, friend requests, a conversation list with unread badges and previews, presence/typing in the chat header, delivery ticks, replies, reactions, image sending, and per-message deletion.
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
