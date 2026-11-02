-- Topic icons: each topic can carry one emoji, shown before its name in the
-- topic row and in the New topic card (#2414). NULL means the default icon.
--
-- Reverse, if this ever has to come out (SQLite 3.35+ supports DROP COLUMN):
--   ALTER TABLE sessions DROP COLUMN topic_icon;

ALTER TABLE sessions ADD COLUMN topic_icon TEXT;
