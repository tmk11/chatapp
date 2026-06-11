# Secure Chat App

A WhatsApp-inspired chat application scaffold with a Rust backend designed for secure, horizontally scalable real-time messaging.

## What is included

- Rust backend using Axum and Tokio.
- User-friendly dark-mode web frontend served by the backend.
- JWT-based authentication with Argon2 password hashing.
- WebSocket real-time messaging endpoint.
- Security middleware for HTTP headers and request-size limits.
- In-memory development stores that can be replaced by Postgres, Redis, and object storage.
- Architecture and AI-agent context in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md).

## Quick start

```bash
cd backend
cp .env.example .env
cargo run
```

Then open `http://127.0.0.1:8080/`, register or login with a phone number in E.164 format, create or choose a room, and start chatting. The frontend uses the same `/auth/*`, `/rooms`, `/me`, and `/ws` backend endpoints.

The backend listens on `127.0.0.1:8080` by default and serves the frontend at `http://127.0.0.1:8080/`.

## Frontend

The `frontend/` directory contains a static, responsive chat client with a dark-first design:

- Login and registration tabs.
- Auth-first screen that hides rooms until the user is logged in.
- Room creation and room list after login.
- Session-scoped development token storage, cleared when the browser tab/session ends.
- Room selection with WebSocket status.
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

### List rooms

```bash
curl http://127.0.0.1:8080/rooms \
  -H 'authorization: Bearer <JWT>'
```

### Create room

```bash
curl -X POST http://127.0.0.1:8080/rooms \
  -H 'authorization: Bearer <JWT>' \
  -H 'content-type: application/json' \
  -d '{"name":"Family chat"}'
```

### WebSocket

Connect to:

```text
ws://127.0.0.1:8080/ws?room_id=<ROOM_ID>&token=<JWT>
```

Send JSON messages like:

```json
{"body":"hello"}
```

## Production roadmap

The current code is a secure development baseline. Before serving real users, complete the production tasks in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md), especially persistent storage, end-to-end encryption, distributed fanout, push notifications, abuse prevention, observability, and load testing.
