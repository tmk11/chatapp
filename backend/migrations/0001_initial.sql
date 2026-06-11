-- Planned production schema. The current development server uses in-memory storage.
-- Apply this migration when the SQLx Postgres adapter is implemented.

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    phone TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS devices (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_name TEXT NOT NULL,
    public_identity_key BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS friend_requests (
    id UUID PRIMARY KEY,
    from_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    to_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (from_user_id, to_user_id),
    CHECK (from_user_id <> to_user_id)
);

-- Friendships store the unordered pair as (least, greatest) user id.
CREATE TABLE IF NOT EXISTS friendships (
    user_a UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_b UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_a, user_b),
    CHECK (user_a < user_b)
);

CREATE TABLE IF NOT EXISTS conversations (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('direct', 'group')),
    title TEXT,
    created_by UUID REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS conversation_members (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('member', 'admin', 'owner')),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (conversation_id, user_id)
);

CREATE TABLE IF NOT EXISTS messages (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    sender_id UUID NOT NULL REFERENCES users(id),
    client_message_id TEXT NOT NULL,
    encrypted_payload BYTEA NOT NULL,
    sent_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ,
    UNIQUE (conversation_id, sender_id, client_message_id)
);

CREATE INDEX IF NOT EXISTS messages_conversation_sent_at_idx
    ON messages (conversation_id, sent_at DESC);

-- Per-user "delete for me". "Delete for everyone" uses messages.deleted_at.
CREATE TABLE IF NOT EXISTS message_deletions (
    message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    deleted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (message_id, user_id)
);
