# Secure Chat App

A WhatsApp-inspired chat application scaffold with a Rust backend designed for secure, horizontally scalable real-time messaging.

## What is included

- Rust backend using Axum and Tokio.
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

The backend listens on `127.0.0.1:8080` by default.

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

### WebSocket

Connect to:

```text
ws://127.0.0.1:8080/ws?room_id=demo&token=<JWT>
```

Send JSON messages like:

```json
{"body":"hello"}
```

## Production roadmap

The current code is a secure development baseline. Before serving real users, complete the production tasks in [`PROJECT_CONTEXT.md`](PROJECT_CONTEXT.md), especially persistent storage, end-to-end encryption, distributed fanout, push notifications, abuse prevention, observability, and load testing.
