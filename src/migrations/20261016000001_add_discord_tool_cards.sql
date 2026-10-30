-- Durable Discord tool cards (#1880). One row per card message, holding the
-- card's state as JSON, so a card can be paged, logged and expanded after a
-- restart. message_id is a Discord snowflake, kept as text.
CREATE TABLE IF NOT EXISTS discord_tool_cards (
    message_id TEXT PRIMARY KEY,
    state TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
