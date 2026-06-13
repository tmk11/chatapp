-- Unify 1:1 and group chats under a conversation model. Existing 1:1 messages
-- are migrated into direct conversations. Read receipts move from per-message
-- columns to per-member read cursors (conversation_members.last_read_at).
--
-- Rollback notes: this migration is destructive to the old 1:1 message shape
-- (drops messages.recipient_id/delivered_at/read_at and attachments.recipient_id).
-- To roll back, restore from a backup taken before applying it.

-- Receipt columns/indexes superseded by conversation_members.last_read_at.
DROP INDEX IF EXISTS messages_unread_idx;
DROP INDEX IF EXISTS messages_conversation_idx;

ALTER TABLE users
    ADD COLUMN avatar_attachment_id UUID REFERENCES attachments(id) ON DELETE SET NULL;

CREATE TABLE conversations (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('direct', 'group')),
    title TEXT,
    avatar_attachment_id UUID REFERENCES attachments(id) ON DELETE SET NULL,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE conversation_members (
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('owner', 'admin', 'member')),
    last_read_at TIMESTAMPTZ,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    position BIGSERIAL,
    PRIMARY KEY (conversation_id, user_id)
);
CREATE INDEX conversation_members_user_idx ON conversation_members (user_id);

-- Stable unique key on the unordered pair for direct conversations.
CREATE TABLE direct_conversations (
    user_a UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_b UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    PRIMARY KEY (user_a, user_b),
    CHECK (user_a < user_b)
);

ALTER TABLE messages
    ADD COLUMN conversation_id UUID REFERENCES conversations(id) ON DELETE CASCADE;
ALTER TABLE messages ADD COLUMN pinned BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE messages ADD COLUMN duration_ms INTEGER;
ALTER TABLE messages DROP CONSTRAINT IF EXISTS messages_kind_check;
ALTER TABLE messages ADD CONSTRAINT messages_kind_check CHECK (kind IN ('text', 'image', 'voice'));

ALTER TABLE attachments ADD COLUMN used BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE attachments SET used = TRUE WHERE recipient_id IS NOT NULL;

-- Backfill: one direct conversation per existing message pair.
CREATE TEMPORARY TABLE pair_map AS
SELECT a, b, gen_random_uuid() AS conversation_id
FROM (
    SELECT DISTINCT LEAST(sender_id, recipient_id) AS a, GREATEST(sender_id, recipient_id) AS b
    FROM messages
) pairs;

INSERT INTO conversations (id, kind, created_at)
SELECT conversation_id, 'direct', now() FROM pair_map;

INSERT INTO direct_conversations (user_a, user_b, conversation_id)
SELECT a, b, conversation_id FROM pair_map;

INSERT INTO conversation_members (conversation_id, user_id, role)
SELECT conversation_id, a, 'member' FROM pair_map
UNION ALL
SELECT conversation_id, b, 'member' FROM pair_map;

UPDATE messages m
SET conversation_id = pm.conversation_id
FROM pair_map pm
WHERE LEAST(m.sender_id, m.recipient_id) = pm.a
  AND GREATEST(m.sender_id, m.recipient_id) = pm.b;

-- Carry over read state: a member's cursor is the latest message they had read.
UPDATE conversation_members cm
SET last_read_at = sub.max_read
FROM (
    SELECT conversation_id, recipient_id AS user_id, MAX(read_at) AS max_read
    FROM messages
    WHERE read_at IS NOT NULL
    GROUP BY conversation_id, recipient_id
) sub
WHERE cm.conversation_id = sub.conversation_id AND cm.user_id = sub.user_id;

ALTER TABLE messages ALTER COLUMN conversation_id SET NOT NULL;
ALTER TABLE messages DROP COLUMN recipient_id;
ALTER TABLE messages DROP COLUMN delivered_at;
ALTER TABLE messages DROP COLUMN read_at;
ALTER TABLE attachments DROP COLUMN recipient_id;

CREATE INDEX messages_conversation_idx ON messages (conversation_id, sent_at);
CREATE INDEX messages_pinned_idx ON messages (conversation_id) WHERE pinned;
