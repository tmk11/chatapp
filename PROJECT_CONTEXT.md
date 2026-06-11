# Secure Chat App — Goals, Context, Rules, and Roadmap

This file is the handoff document for future AI agents and engineers. Read it before making changes.

## Product goal

Build a WhatsApp-like chat app with strong security, reliable real-time messaging, and an architecture that can scale toward 1 million users.

Core user-facing capabilities:

1. Account registration and login by phone number.
2. 1:1 and group chats.
3. Real-time text messaging.
4. Message delivery/read receipts.
5. Offline message sync.
6. Media attachments.
7. Push notifications.
8. Contact discovery with privacy controls.
9. End-to-end encryption for message content.
10. Abuse prevention, rate limiting, reporting, and moderation workflows.

## Current implementation

The repository currently contains a Rust backend scaffold in `backend/` plus a static development frontend in `frontend/`:

- Axum HTTP server and Tokio async runtime.
- JWT authentication.
- Argon2 password hashing.
- Development-only in-memory user store.
- Development-only in-memory room store with authenticated room creation and listing. Room names are unique case-insensitively in this development store so two users typing the same room name land in the same room.
- Development-only in-memory WebSocket room fanout.
- Basic health, auth, profile, room, and WebSocket endpoints.
- Static dark-first web frontend with an auth-first flow: users only see login/signup first, then room creation, room selection, and realtime messaging after login.
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

1. Replace in-memory user storage with Postgres via SQLx migrations.
2. Add refresh tokens, device sessions, logout, secure browser session handling, and token revocation.
3. Add Redis-backed distributed rate limits.
4. Replace in-memory rooms with Postgres conversation and membership data models.
5. Add room membership, ownership controls, invites, and authorization checks per room.
6. Add durable encrypted message storage.
7. Add message IDs generated by clients for idempotency.
8. Add distributed fanout with NATS, Kafka, or Redpanda.
9. Add delivery and read receipts.
10. Add offline sync with pagination and per-device cursors.
11. Add end-to-end encryption protocol support.
12. Add OpenTelemetry tracing and Prometheus metrics.
13. Add Docker Compose for local Postgres, Redis, and broker dependencies.
14. Add CI with formatting, clippy, tests, audit, and container scan.
15. Add production frontend build pipeline, CSP, secure cookie/session strategy, and frontend integration tests.
16. Add load-test scenarios for 10k, 100k, and 1M-user growth phases.

## Definition of done for backend changes

A backend change is done only when:

- `cargo fmt --check` passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes or the limitation is documented.
- `cargo test --all` passes.
- New behavior has tests or a written reason tests are deferred.
- Security and privacy impacts are considered, including browser token handling for frontend changes.
- This context file is updated if architecture, roadmap, or rules changed.
