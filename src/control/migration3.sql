CREATE UNIQUE INDEX active_integration ON integrations(run)
WHERE state IN ('preparing','checking','awaiting_review','verified','applying');
CREATE TABLE repository_bindings (
  run TEXT PRIMARY KEY REFERENCES runs(id), path TEXT NOT NULL, common_dir TEXT NOT NULL,
  ref TEXT NOT NULL, state TEXT NOT NULL, active INTEGER NOT NULL CHECK(active IN (0,1))
) STRICT;
CREATE UNIQUE INDEX one_repository_authority ON repository_bindings(common_dir) WHERE active=1;
CREATE TABLE review_assignments (
  attempt TEXT PRIMARY KEY REFERENCES attempts(id), integration TEXT NOT NULL REFERENCES integrations(id)
) STRICT;
CREATE TABLE check_runs (
  id TEXT PRIMARY KEY, integration TEXT NOT NULL REFERENCES integrations(id),
  name TEXT NOT NULL, state TEXT NOT NULL, doc TEXT NOT NULL,
  UNIQUE(integration,name)
) STRICT;
CREATE TABLE verification_worktrees (
  integration TEXT PRIMARY KEY REFERENCES integrations(id), path TEXT NOT NULL UNIQUE,
  state TEXT NOT NULL
) STRICT;
