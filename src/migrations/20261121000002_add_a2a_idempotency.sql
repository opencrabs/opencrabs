-- Caller-side retry contract (#2874): a resend of the same message must not
-- create a second task. The key is the A2A `messageId` the caller minted, so
-- no new wire field is invented; a send without one behaves exactly as it
-- always has.

CREATE TABLE IF NOT EXISTS a2a_idempotency (
    key TEXT PRIMARY KEY NOT NULL,      -- caller-minted messageId
    task_id TEXT NOT NULL,              -- the task the first attempt created
    created_at INTEGER NOT NULL         -- Unix timestamp, swept by age
);

CREATE INDEX IF NOT EXISTS idx_a2a_idempotency_created ON a2a_idempotency(created_at);
