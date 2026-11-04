-- #2027: the ledger stored a token total and a cost, nothing else, so the
-- dashboard could not say how much of a model's tokens were cache reads. Keep
-- the cache-read count per row. The column is NULL, never 0, on every row
-- written before it existed: those rows carry no split at all, and a 0 would
-- claim the provider reported no cache reads when it reported nothing.
ALTER TABLE usage_ledger ADD COLUMN cache_read_tokens INTEGER;
