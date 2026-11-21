-- Hint card: a dismissed line stays quiet for a window instead of forever.
--
-- A dismissal is a property of THE READER, not of the session and not of the
-- conversation: the tips exist to be discovered once, and someone who hid "meet
-- the Seeker" does not want to meet it again in the next chat either. So the
-- row is global, keyed by the hint id and nothing else.
--
-- Ids, not the printed line. The copy is exactly what gets reworded, and a
-- store keyed by the sentence would resurrect a dismissed tip the first time a
-- word changed, which reads to the reader as the dismissal not working.
--
-- `until_at` is an absolute unix timestamp rather than "dismissed at", so the
-- read is one comparison and needs no arithmetic per row: expired rows are
-- simply not returned, and the write path sweeps them so the table cannot grow
-- without bound.
CREATE TABLE IF NOT EXISTS hint_dismissals (
    hint_id  TEXT PRIMARY KEY NOT NULL,
    until_at INTEGER NOT NULL
);
