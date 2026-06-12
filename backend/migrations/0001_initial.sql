-- Initial production schema for the secure chat backend.
-- Applied automatically at startup when DATABASE_URL is configured.
--
-- Rollback notes: this is the initial migration; to roll back drop the
-- application schema entirely:
--   DROP TABLE IF EXISTS message_reactions, message_deletions, messages,
--     attachments, friendships, friend_requests, users CASCADE;
--   DROP TABLE IF EXISTS _sqlx_migrations;

CREATE TABLE users (
    id UUID PRIMARY KEY,
    phone TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    last_seen_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE friend_requests (
    id UUID PRIMARY KEY,
    from_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    to_user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (from_user_id, to_user_id),
    CHECK (from_user_id <> to_user_id)
);

CREATE INDEX friend_requests_to_user_idx ON friend_requests (to_user_id, created_at);

-- Friendships store the unordered pair as (least, greatest) user id.
CREATE TABLE friendships (
    user_a UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_b UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_a, user_b),
    CHECK (user_a < user_b)
);

CREATE INDEX friendships_user_b_idx ON friendships (user_b);

-- Image bytes currently live in Postgres; move to encrypted object storage
-- before production scale (see PROJECT_CONTEXT.md roadmap).
CREATE TABLE attachments (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recipient_id UUID REFERENCES users(id) ON DELETE SET NULL,
    content_type TEXT NOT NULL,
    bytes BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE messages (
    id UUID PRIMARY KEY,
    sender_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recipient_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('text', 'image')),
    body TEXT NOT NULL,
    attachment_id UUID REFERENCES attachments(id) ON DELETE SET NULL,
    reply_to UUID REFERENCES messages(id) ON DELETE SET NULL,
    sent_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ,
    read_at TIMESTAMPTZ,
    deleted_for_everyone BOOLEAN NOT NULL DEFAULT FALSE
);

-- Conversation scans always address the unordered participant pair.
CREATE INDEX messages_conversation_idx
    ON messages (LEAST(sender_id, recipient_id), GREATEST(sender_id, recipient_id), sent_at);

-- Unread lookups: messages to me that are not yet read.
CREATE INDEX messages_unread_idx ON messages (recipient_id) WHERE read_at IS NULL;

-- Per-user "delete for me". "Delete for everyone" uses messages.deleted_for_everyone.
CREATE TABLE message_deletions (
    message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    deleted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (message_id, user_id)
);

CREATE TABLE message_reactions (
    message_id UUID NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    emoji TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (message_id, user_id, emoji)
);
