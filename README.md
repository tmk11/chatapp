# Secure Chat App

A WhatsApp-inspired chat application scaffold with a Rust backend designed for secure, horizontally scalable real-time messaging.

## What is included

- Rust backend using Axum and Tokio.
- User-friendly dark-mode web frontend served by the backend.
- JWT-based authentication with Argon2 password hashing.
- WhatsApp-style friend requests: add a friend by phone number, accept or decline incoming requests.
- 1:1 real-time messaging over WebSocket between friends only.
- Stored conversation history that survives reconnects (in-memory development store).
- Message deletion: delete for me (hides the message for you) or delete for everyone (sender only, tombstones the message for both sides).
- Security middleware for HTTP headers and request-size limits.
- In-memory development stores that can be replaced by Postgres, Redis, and object storage.
- Architecture and AI-agent context in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md).

## Quick start

```bash
cd backend
cp .env.example .env
cargo run
```

Then open `http://127.0.0.1:8080/`, register or login with a phone number in E.164 format, add a friend by their phone number, wait for them to accept, and start chatting. The frontend uses the same `/auth/*`, `/friends*`, `/messages*`, `/me`, and `/ws` backend endpoints.

The backend listens on `127.0.0.1:8080` by default and serves the frontend at `http://127.0.0.1:8080/`.

## Frontend

The `frontend/` directory contains a static, responsive chat client with a dark-first design:

- Login and registration tabs.
- Auth-first screen that hides contacts until the user is logged in.
- Add-friend form, incoming friend-request list with accept/decline, and friends list.
- Session-scoped development token storage, cleared when the browser tab/session ends.
- One WebSocket connection per session with live status.
- Stored conversation history loaded when a friend is selected.
- Per-message actions: delete for me, and delete for everyone on your own messages.
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

### List friends

```bash
curl http://127.0.0.1:8080/friends \
  -H 'authorization: Bearer <JWT>'
```

### Conversation history

```bash
curl http://127.0.0.1:8080/messages/<FRIEND_USER_ID> \
  -H 'authorization: Bearer <JWT>'
```

Returns the stored conversation oldest-first. Messages you deleted for yourself
are omitted; messages deleted for everyone are returned with `"deleted": true`
and an empty body.

### Delete a message

```bash
# Hide the message for yourself only (any participant):
curl -X DELETE 'http://127.0.0.1:8080/messages/<MESSAGE_ID>?scope=me' \
  -H 'authorization: Bearer <JWT>'

# Delete for everyone (sender only); both sides see a tombstone and connected
# clients receive a `message_deleted` WebSocket event:
curl -X DELETE 'http://127.0.0.1:8080/messages/<MESSAGE_ID>?scope=everyone' \
  -H 'authorization: Bearer <JWT>'
```

### WebSocket

Connect once per session:

```text
ws://127.0.0.1:8080/ws?token=<JWT>
```

Send direct messages to a friend:

```json
{"to":"<FRIEND_USER_ID>","body":"hello"}
```

Receive events shaped like:

```json
{"type":"message","message":{"id":"...","sender_id":"...","recipient_id":"...","body":"hello","sent_at":"...","deleted":false}}
{"type":"message_deleted","message_id":"...","sender_id":"...","recipient_id":"..."}
{"type":"error","error":"recipient is not your friend"}
```

Messages can only be exchanged between users who are friends.

## Production roadmap

The current code is a secure development baseline. Before serving real users, complete the production tasks in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md), especially persistent storage, end-to-end encryption, distributed fanout, push notifications, abuse prevention, observability, and load testing.
