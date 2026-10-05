CREATE TABLE coordinator_launches (
  id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES runs(id),
  generation INTEGER NOT NULL, state TEXT NOT NULL, doc TEXT NOT NULL,
  last_error TEXT NOT NULL DEFAULT '', UNIQUE(run,generation)
) STRICT;
CREATE TABLE coordinator_processes (
  launch TEXT PRIMARY KEY REFERENCES coordinator_launches(id),
  generation INTEGER NOT NULL, state TEXT NOT NULL, doc TEXT NOT NULL
) STRICT;
