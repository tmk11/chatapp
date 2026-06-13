# Secure Chat App

A WhatsApp-inspired chat application scaffold with a Rust backend designed for secure, horizontally scalable real-time messaging.

## What is included

- Rust backend using Axum and Tokio.
- Fresh, friendly "Ripple" web frontend (light-first with automatic dark mode) served by the backend.
- JWT-based authentication with Argon2 password hashing.
- Durable Postgres storage (users, friendships, conversations, members, messages, reactions, attachments) when `DATABASE_URL` is set, with automatic migrations at startup; in-memory development stores otherwise.
- WhatsApp-style friend requests: add a friend by phone number, accept or decline incoming requests.
- Unified conversations: 1:1 **direct** chats (created on demand between friends) and **group** chats (a title, an owner, add/remove members) over one WebSocket.
- Read receipts via per-member read cursors: ✓ sent and ✓✓ read in direct chats, "👁 N" seen counts in groups; plus typing indicators and online/offline presence with last-seen.
- Conversation list sorted by latest activity with unread counts and previews.
- Replies (quoted messages) and emoji reactions (👍 ❤️ 😂 😮 😢 🙏).
- Image messages and **voice messages** (record in the browser); media is validated from magic bytes server-side and readable only by conversation members.
- **Pin** important messages (pinned bar at the top of the chat) and **search** within a conversation.
- Profile photos: upload an avatar image; shown across the app with a colourful gradient fallback.
- Stored conversation history that survives reconnects and, with Postgres, server restarts.
- Message deletion: delete for me (hides the message for you) or delete for everyone (sender only, tombstones the message for all members).
- Security middleware for HTTP headers and request-size limits.
- Architecture and AI-agent context in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md).

## Quick start

```bash
cd backend
cp .env.example .env
cargo run
```

Then open `http://127.0.0.1:8080/`, register or login with a phone number in E.164 format, add a friend by their phone number, then press ✎ to start a direct chat or create a group. The frontend uses the `/auth/*`, `/friends*`, `/conversations*`, `/messages/*`, `/attachments*`, `/me*`, and `/ws` backend endpoints.

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

The `frontend/` directory contains a static, responsive chat client ("Ripple") with a fresh, youthful design — a vibrant indigo→violet→pink brand gradient, rounded cards, colourful per-user avatars, and chat bubbles. It is light-first and automatically switches to a dark palette via `prefers-color-scheme`. On phones it collapses to a single pane with a back button between the conversation list and the open chat.

- Login and registration tabs with a split-screen welcome panel.
- Add-friend form and incoming friend-request list with accept/decline.
- Conversation list (directs + groups) sorted by latest activity, with avatars, online dots, previews, timestamps, and unread badges.
- A ✎ "new chat" dialog to start a direct chat with a friend or create a group (title + member picker); a group-info dialog to add members or leave.
- Chat header with presence/typing (directs) or member count (groups), a pinned-messages bar, and in-conversation search.
- Read status on your own messages (✓ / ✓✓ in directs, "👁 N" in groups); conversations marked read when opened.
- Reply, react (👍 ❤️ 😂 😮 😢 🙏), pin/unpin, delete-for-me and delete-for-everyone per message.
- Image sending via 📷 and voice messages via 🎙️ (MediaRecorder); media rendered inline with an audio player.
- Profile photo upload by tapping your own avatar.
- Session-scoped development token storage, one WebSocket per session, accessible live log and toasts.

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

### Friends list

`GET /friends` returns the caller's friends with presence (used to start
direct chats and pick group members). The chat list itself is `/conversations`.

### Conversation list

```bash
curl http://127.0.0.1:8080/conversations -H 'authorization: Bearer <JWT>'
```

Returns directs + groups, newest activity first. Each entry has `kind`
(`direct`|`group`), `title` (group), `other_user` (direct), `members`
(each with `online`), `unread_count`, and `last_message`.

### Start a direct chat / create a group

```bash
# Get or create the direct conversation with a friend:
curl -X POST http://127.0.0.1:8080/conversations/direct \
  -H 'authorization: Bearer <JWT>' -H 'content-type: application/json' \
  -d '{"user_id":"<FRIEND_USER_ID>"}'

# Create a group (creator becomes owner):
curl -X POST http://127.0.0.1:8080/conversations/group \
  -H 'authorization: Bearer <JWT>' -H 'content-type: application/json' \
  -d '{"title":"Squad","member_ids":["<FRIEND_A>","<FRIEND_B>"]}'
```

Group membership: `POST /conversations/{id}/members` `{"user_id":...}` adds a
friend; `DELETE /conversations/{id}/members/{user_id}` leaves (self) or, as
owner, removes a member.

### History, search, and pins

```bash
curl http://127.0.0.1:8080/conversations/<ID>/messages -H 'authorization: Bearer <JWT>'
curl 'http://127.0.0.1:8080/conversations/<ID>/search?q=milk' -H 'authorization: Bearer <JWT>'
curl http://127.0.0.1:8080/conversations/<ID>/pins -H 'authorization: Bearer <JWT>'
```

History is oldest-first; messages you deleted for yourself are omitted and
ones deleted for everyone are returned with `"deleted": true`. Search matches
text messages case-insensitively within the conversation.

### Attachments (images and voice)

```bash
# Upload raw PNG/JPEG/GIF/WebP (image) or WebM/OGG/MP4/MPEG/WAV (audio), max 10 MiB:
curl -X POST http://127.0.0.1:8080/attachments \
  -H 'authorization: Bearer <JWT>' -H 'content-type: image/png' --data-binary @photo.png

# Download (uploader, conversation members, or if it is a profile avatar):
curl http://127.0.0.1:8080/attachments/<ATTACHMENT_ID> -H 'authorization: Bearer <JWT>' -o photo.png
```

The content type is detected from magic bytes, not the header. An attachment
is private to its uploader until sent, and is single-use.

### Profile avatar

```bash
curl -X PUT http://127.0.0.1:8080/me/avatar \
  -H 'authorization: Bearer <JWT>' -H 'content-type: application/json' \
  -d '{"attachment_id":"<IMAGE_ATTACHMENT_ID>"}'
```

### Pin and delete

```bash
curl -X POST http://127.0.0.1:8080/messages/<MESSAGE_ID>/pin \
  -H 'authorization: Bearer <JWT>' -H 'content-type: application/json' -d '{"pinned":true}'

curl -X DELETE 'http://127.0.0.1:8080/messages/<MESSAGE_ID>?scope=me' -H 'authorization: Bearer <JWT>'
curl -X DELETE 'http://127.0.0.1:8080/messages/<MESSAGE_ID>?scope=everyone' -H 'authorization: Bearer <JWT>'
```

### WebSocket

Connect once per session: `ws://127.0.0.1:8080/ws?token=<JWT>`. All frames are
JSON tagged with `type`. Client → server:

```json
{"type":"message","conversation_id":"<ID>","body":"hello","reply_to":null}
{"type":"message","conversation_id":"<ID>","kind":"image","attachment_id":"<ATT>"}
{"type":"message","conversation_id":"<ID>","kind":"voice","attachment_id":"<ATT>","duration_ms":4200}
{"type":"read","conversation_id":"<ID>"}
{"type":"typing","conversation_id":"<ID>"}
{"type":"reaction","message_id":"<MESSAGE_ID>","emoji":"👍"}
```

`message` defaults to `kind:"text"` (carry `body`); image/voice carry an
`attachment_id` (voice also `duration_ms`). `read` advances the caller's read
cursor. Server → client (fanned out to conversation members):

```json
{"type":"message","message":{"id":"...","conversation_id":"...","sender_id":"...","kind":"text","body":"hello","attachment_id":null,"duration_ms":null,"reply_to":null,"reactions":[],"pinned":false,"read_by":[],"sent_at":"...","deleted":false}}
{"type":"message_deleted","conversation_id":"...","message_id":"..."}
{"type":"message_pinned","conversation_id":"...","message_id":"...","pinned":true}
{"type":"read","conversation_id":"...","user_id":"...","at":"..."}
{"type":"typing","conversation_id":"...","from":"<USER_ID>"}
{"type":"presence","user_id":"...","online":false,"last_seen_at":"..."}
{"type":"reaction","conversation_id":"...","message_id":"...","user_id":"...","emoji":"👍","added":true}
{"type":"conversation_updated","conversation_id":"..."}
```

`read_by` is the set of other members who have read the message. Presence
events are sent to a user's friends on first-connect/last-disconnect.
Sending, typing, and reacting require conversation membership.

## Production roadmap

The current code is a secure development baseline. Before serving real users, complete the production tasks in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md), especially persistent storage, end-to-end encryption, distributed fanout, push notifications, abuse prevention, observability, and load testing.
