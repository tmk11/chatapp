# Secure Chat App

A WhatsApp-inspired chat application scaffold with a Rust backend designed for secure, horizontally scalable real-time messaging.

## What is included

- Rust backend using Axum and Tokio.
- User-friendly dark-mode web frontend served by the backend.
- JWT-based authentication with Argon2 password hashing.
- Durable Postgres storage (users, friendships, messages, reactions, attachments) when `DATABASE_URL` is set, with automatic migrations at startup; in-memory development stores otherwise.
- WhatsApp-style friend requests: add a friend by phone number, accept or decline incoming requests.
- 1:1 real-time messaging over WebSocket between friends only.
- Delivery and read receipts (✓ sent, ✓✓ delivered, blue ✓✓ read).
- Typing indicators and online/offline presence with last-seen timestamps.
- Conversation list sorted by latest activity with unread counts and message previews.
- Replies (quoted messages) and emoji reactions (👍 ❤️ 😂 😮 😢 🙏).
- Image messages: upload PNG/JPEG/GIF/WebP up to 5 MiB and send to a friend; image type is validated from magic bytes server-side.
- Stored conversation history that survives reconnects and, with Postgres, server restarts.
- Message deletion: delete for me (hides the message for you) or delete for everyone (sender only, tombstones the message for both sides).
- Security middleware for HTTP headers and request-size limits.
- Architecture and AI-agent context in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md).

## Quick start

```bash
cd backend
cp .env.example .env
cargo run
```

Then open `http://127.0.0.1:8080/`, register or login with a phone number in E.164 format, add a friend by their phone number, wait for them to accept, and start chatting. The frontend uses the same `/auth/*`, `/friends*`, `/messages*`, `/attachments*`, `/me`, and `/ws` backend endpoints.

The backend listens on `127.0.0.1:8080` by default and serves the frontend at `http://127.0.0.1:8080/`.

### Durable storage with Postgres

Without configuration the server runs on in-memory development stores (all
data is lost on restart). To keep data durably, point `DATABASE_URL` at a
Postgres database; migrations in `backend/migrations/` run automatically at
startup:

```bash
DATABASE_URL=postgres://chatapp:chatapp@127.0.0.1:5432/chatapp cargo run
```

`docker-compose.yml` starts Postgres alongside the backend with this wiring
already in place.

## Frontend

The `frontend/` directory contains a static, responsive chat client with a dark-first design:

- Login and registration tabs.
- Auth-first screen that hides contacts until the user is logged in.
- Add-friend form, incoming friend-request list with accept/decline.
- WhatsApp-style conversation list: sorted by latest activity, with online dots, message previews, timestamps, and unread badges.
- Chat header presence line: online, last seen, or typing indicator.
- Delivery ticks on your own messages (✓ sent, ✓✓ delivered, blue ✓✓ read); conversations are marked read when opened.
- Reply to any message (quoted block in the bubble, Esc or ✕ to cancel) and react with 👍 ❤️ 😂 😮 😢 🙏 via the React action or by tapping existing reaction chips.
- Session-scoped development token storage, cleared when the browser tab/session ends.
- One WebSocket connection per session with live status.
- Stored conversation history loaded when a friend is selected.
- Image sending via the 📷 button; images are fetched with the auth token and rendered inline.
- Per-message actions: reply, react, delete for me, and delete for everyone on your own messages.
- Accessible live message log and toast feedback.

Set `FRONTEND_DIR` if you want the backend to serve a different static asset directory.

## API

### Health

```bash
curl http://127.0.0.1:8080/health
```

### Register

```bash
curl -X POST http://127.0.0.1:8080/auth/register \
  -H 'content-type: application/json' \
  -d '{"phone":"+15550001111","display_name":"Alice","password":"correct horse battery staple"}'
```

### Login

```bash
curl -X POST http://127.0.0.1:8080/auth/login \
  -H 'content-type: application/json' \
  -d '{"phone":"+15550001111","password":"correct horse battery staple"}'
```

### Send a friend request

```bash
curl -X POST http://127.0.0.1:8080/friends/requests \
  -H 'authorization: Bearer <JWT>' \
  -H 'content-type: application/json' \
  -d '{"phone":"+15550002222"}'
```

Returns `{"status":"pending"}`, or `{"status":"accepted"}` when the other user
had already sent you a request (mutual requests become a friendship immediately).

### List incoming friend requests

```bash
curl http://127.0.0.1:8080/friends/requests \
  -H 'authorization: Bearer <JWT>'
```

### Accept or decline a friend request

```bash
curl -X POST http://127.0.0.1:8080/friends/requests/<REQUEST_ID> \
  -H 'authorization: Bearer <JWT>' \
  -H 'content-type: application/json' \
  -d '{"accept":true}'
```

### Conversation list

```bash
curl http://127.0.0.1:8080/friends \
  -H 'authorization: Bearer <JWT>'
```

Returns one entry per friend, sorted by latest activity, shaped like:

```json
[
  {
    "user": {"id":"...","phone":"+15550002222","display_name":"Bob","last_seen_at":"...","created_at":"..."},
    "online": true,
    "unread_count": 2,
    "last_message": {"id":"...","kind":"text","body":"see you soon","sent_at":"...", "...": "..."}
  }
]
```

### Conversation history

```bash
curl http://127.0.0.1:8080/messages/<FRIEND_USER_ID> \
  -H 'authorization: Bearer <JWT>'
```

Returns the stored conversation oldest-first. Messages you deleted for yourself
are omitted; messages deleted for everyone are returned with `"deleted": true`
and an empty body.

### Upload an image attachment

```bash
curl -X POST http://127.0.0.1:8080/attachments \
  -H 'authorization: Bearer <JWT>' \
  -H 'content-type: image/png' \
  --data-binary @photo.png
```

Accepts raw PNG, JPEG, GIF, or WebP bytes up to 5 MiB; the image type is
detected from the file's magic bytes, not the header. Returns
`{"id":"<ATTACHMENT_ID>","content_type":"image/png","size":...}`. The
attachment stays private to you until you send it in a message, and can be
used in exactly one message.

### Download an image attachment

```bash
curl http://127.0.0.1:8080/attachments/<ATTACHMENT_ID> \
  -H 'authorization: Bearer <JWT>' -o photo.png
```

Only the uploader and, after the message is sent, the recipient can download
an attachment.

### Delete a message

```bash
# Hide the message for yourself only (any participant):
curl -X DELETE 'http://127.0.0.1:8080/messages/<MESSAGE_ID>?scope=me' \
  -H 'authorization: Bearer <JWT>'

# Delete for everyone (sender only); both sides see a tombstone, any image
# attachment is purged, and connected clients receive a `message_deleted`
# WebSocket event:
curl -X DELETE 'http://127.0.0.1:8080/messages/<MESSAGE_ID>?scope=everyone' \
  -H 'authorization: Bearer <JWT>'
```

### WebSocket

Connect once per session:

```text
ws://127.0.0.1:8080/ws?token=<JWT>
```

All frames are JSON objects tagged with `type`. Client → server:

```json
{"type":"message","to":"<FRIEND_USER_ID>","body":"hello","reply_to":null}
{"type":"message","to":"<FRIEND_USER_ID>","attachment_id":"<ATTACHMENT_ID>"}
{"type":"delivered","message_ids":["<MESSAGE_ID>"]}
{"type":"read","peer_id":"<FRIEND_USER_ID>"}
{"type":"typing","to":"<FRIEND_USER_ID>"}
{"type":"reaction","message_id":"<MESSAGE_ID>","emoji":"👍"}
```

- `message` carries exactly one of `body` or `attachment_id`; the optional
  `reply_to` must reference a message in the same conversation.
- `delivered` acks receipt of specific messages (recipient only).
- `read` marks everything unread from `peer_id` as read and delivered.
- `reaction` toggles one of 👍 ❤️ 😂 😮 😢 🙏 on or off.

Server → client:

```json
{"type":"message","message":{"id":"...","sender_id":"...","recipient_id":"...","kind":"text","body":"hello","attachment_id":null,"reply_to":null,"reactions":[],"sent_at":"...","delivered_at":null,"read_at":null,"deleted":false}}
{"type":"message_deleted","message_id":"...","sender_id":"...","recipient_id":"..."}
{"type":"delivered","message_ids":["..."],"by":"<USER_ID>"}
{"type":"read","message_ids":["..."],"by":"<USER_ID>"}
{"type":"typing","from":"<USER_ID>"}
{"type":"presence","user_id":"<USER_ID>","online":false,"last_seen_at":"..."}
{"type":"reaction","message_id":"...","user_id":"...","emoji":"👍","added":true}
```

`reply_to`, when present, embeds a preview of the quoted message:
`{"id":"...","sender_id":"...","kind":"text","body":"truncated snapshot","deleted":false}`.
Presence events are sent to a user's friends when their first connection
opens or their last connection closes. Messages, typing signals, and
reactions can only be exchanged between users who are friends.

## Production roadmap

The current code is a secure development baseline. Before serving real users, complete the production tasks in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md), especially persistent storage, end-to-end encryption, distributed fanout, push notifications, abuse prevention, observability, and load testing.
