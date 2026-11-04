-- Topics: a topic is a session inside a session (#2301).
--
-- A topic reuses everything a session already has (transcript, title, id,
-- chat binding), so the only new state is the link to the parent row and two
-- flags. Existing rows are untouched: parent_id is NULL and topics_enabled is 0.
--
-- Depth is one. The triggers refuse an insert or a re-parent whose parent is
-- itself a topic, so no surface can render an unbounded tree.
--
-- Deleting a parent cascades to its topics (foreign_keys is on, see
-- src/db/database.rs), so no topic is ever orphaned.
--
-- Reverse, if this ever has to come out (SQLite 3.35+ supports DROP COLUMN):
--   DROP TRIGGER IF EXISTS sessions_topic_depth_update;
--   DROP TRIGGER IF EXISTS sessions_topic_depth_insert;
--   DROP INDEX IF EXISTS idx_sessions_parent_id;
--   ALTER TABLE sessions DROP COLUMN closed_at;
--   ALTER TABLE sessions DROP COLUMN topics_enabled;
--   ALTER TABLE sessions DROP COLUMN parent_id;

ALTER TABLE sessions ADD COLUMN parent_id TEXT REFERENCES sessions(id) ON DELETE CASCADE;
ALTER TABLE sessions ADD COLUMN topics_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN closed_at INTEGER;

CREATE INDEX IF NOT EXISTS idx_sessions_parent_id ON sessions(parent_id);

CREATE TRIGGER IF NOT EXISTS sessions_topic_depth_insert
BEFORE INSERT ON sessions
WHEN NEW.parent_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM sessions p WHERE p.id = NEW.parent_id AND p.parent_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'topic depth is one: the parent is already a topic');
END;

CREATE TRIGGER IF NOT EXISTS sessions_topic_depth_update
BEFORE UPDATE OF parent_id ON sessions
WHEN NEW.parent_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM sessions p WHERE p.id = NEW.parent_id AND p.parent_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'topic depth is one: the parent is already a topic');
END;
