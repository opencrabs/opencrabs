-- Reactions on stored transcript rows (#2825).
--
-- A reaction here is local state. The rows it lands on are the ones this app
-- stored itself, so nothing is sent to Telegram or Discord and no transport is
-- consulted: these messages never existed on those transports.
--
-- The primary key is (message_id, emoji), so reacting twice with the same
-- picture is one reaction, not two. The write path upserts and the toggle
-- deletes, which is what makes a second click take the reaction away.
--
-- Foreign keys are on (`PRAGMA foreign_keys = ON` in `src/db/database.rs`) and
-- the table carries `ON DELETE CASCADE`, so a message that is pruned drops its
-- reactions with it. No orphan sweep exists and none is needed.
CREATE TABLE IF NOT EXISTS message_reactions (
    message_id TEXT NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    emoji TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    PRIMARY KEY (message_id, emoji)
);
