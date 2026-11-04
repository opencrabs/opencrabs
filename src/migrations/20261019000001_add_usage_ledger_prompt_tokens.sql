-- Prompt tokens per ledger row (#2136): input + cache writes + cache reads for
-- the call. The By Model cache share divides cache reads by this, not by the
-- row's token total, which also counts output. Rows written before this column
-- carry NULL and are left out of the share.
ALTER TABLE usage_ledger ADD COLUMN prompt_tokens INTEGER;
