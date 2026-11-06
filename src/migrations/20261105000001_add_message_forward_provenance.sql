-- Forward provenance on transcript rows (#2608).
--
-- Forwarding a message from one session into another is a COPY, not a
-- reference: the destination keeps reading its own history even if the source
-- session is renamed or deleted, exactly like the Telegram client. Four
-- nullable columns carry that copy's provenance.
--
--   forwarded_from_session  the source session's id, kept for a later
--                           "open the original" affordance.
--   forwarded_from_message  the source message's id, same purpose.
--   forwarded_from_label    the source session's name FROZEN at forward time.
--                           Stored rather than joined so a later rename cannot
--                           rewrite what the reader saw when it arrived.
--   forward_batch_id        one id shared by every row of a multi-select
--                           forward, so the renderer draws one header and one
--                           accent bar over N messages instead of N stacked
--                           headers.
--
-- All four are NULL on every existing row and on every message that was not
-- forwarded, so no heal pass is needed.
ALTER TABLE messages ADD COLUMN forwarded_from_session TEXT;
ALTER TABLE messages ADD COLUMN forwarded_from_message TEXT;
ALTER TABLE messages ADD COLUMN forwarded_from_label TEXT;
ALTER TABLE messages ADD COLUMN forward_batch_id TEXT;

-- A destination session groups its forwards by batch when it loads a window of
-- history, so the batch id needs an index of its own.
CREATE INDEX IF NOT EXISTS idx_messages_forward_batch
    ON messages (session_id, forward_batch_id);