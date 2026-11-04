-- Sessions dialog: favorite a session and pin it to the top of the list.
--
-- A favorite is a property OF THE USER'S VIEW, not of the session, so it lives
-- in a sidecar table instead of a `sessions` column. Two reasons:
--
-- 1. A column add touches every `Session` struct literal in the tree (31 of
--    them, including test fixtures that live outside the db module), which is
--    churn with no payoff for a TUI-only affordance.
-- 2. A separate row means the pin can be written and cleared without an UPDATE
--    on `sessions`, so `updated_at` is never nudged. That matters: unpinning
--    must return the session to its REAL recency slot, and an `updated_at`
--    bump would silently promote it to the top of the recency order forever.
--
-- ON DELETE CASCADE keeps the store honest: the foreign_keys pragma is on
-- (src/db/database.rs), so deleting a session drops its pin with it and no
-- orphan sweep is ever needed.
--
-- The implicit index on the PRIMARY KEY already serves every lookup (single
-- session pin test, the pin/unpin statements, and the favorite set hydrated at
-- load), so no second index is declared.
CREATE TABLE IF NOT EXISTS session_favorites (
    session_id   TEXT PRIMARY KEY NOT NULL
                 REFERENCES sessions(id) ON DELETE CASCADE,
    favorited_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);