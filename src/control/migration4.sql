CREATE TABLE coordinator_routes (
  run TEXT PRIMARY KEY REFERENCES runs(id), generation INTEGER NOT NULL,
  endpoint TEXT NOT NULL, credential TEXT NOT NULL, cursor INTEGER NOT NULL,
  active INTEGER NOT NULL CHECK(active IN (0,1))
) STRICT;
CREATE TABLE coordinator_notices (
  id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES runs(id), generation INTEGER NOT NULL,
  sequence INTEGER NOT NULL, kind TEXT NOT NULL, aggregate_id TEXT NOT NULL,
  endpoint TEXT NOT NULL, body TEXT NOT NULL, state TEXT NOT NULL,
  tries INTEGER NOT NULL DEFAULT 0, available_at INTEGER NOT NULL,
  receipt TEXT, last_error TEXT NOT NULL DEFAULT '', acknowledged_by TEXT,
  prepared_body TEXT,
  sender_origin TEXT,
  UNIQUE(run,generation,sequence)
) STRICT;
