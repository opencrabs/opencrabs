-- Collapsed topic trees: a session's topics fold under it in both the GUI and
-- the TUI, and the choice is kept per session (#2427). 0 means the tree is open.
--
-- Reverse, if this ever has to come out (SQLite 3.35+ supports DROP COLUMN):
--   ALTER TABLE sessions DROP COLUMN topics_collapsed;

ALTER TABLE sessions ADD COLUMN topics_collapsed INTEGER NOT NULL DEFAULT 0;
