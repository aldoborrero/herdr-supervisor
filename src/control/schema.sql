CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
CREATE TABLE principals (
  id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, role TEXT NOT NULL,
  run TEXT, attempt TEXT, node TEXT, generation INTEGER NOT NULL,
  active INTEGER NOT NULL CHECK(active IN (0,1))
) STRICT;
CREATE TABLE nodes (
  id TEXT PRIMARY KEY, key TEXT NOT NULL UNIQUE, epoch INTEGER NOT NULL,
  max_active INTEGER NOT NULL CHECK(max_active>0), active INTEGER NOT NULL
) STRICT;
CREATE TABLE runs (
  id TEXT PRIMARY KEY, project TEXT NOT NULL, repo TEXT NOT NULL,
  state TEXT NOT NULL, revision INTEGER NOT NULL, doc TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX active_repository ON runs(repo)
WHERE state NOT IN ('completed','cancelled');
CREATE TABLE tasks (
  id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES runs(id), state TEXT NOT NULL,
  revision INTEGER NOT NULL, doc TEXT NOT NULL
) STRICT;
CREATE TABLE dependencies (
  task TEXT NOT NULL REFERENCES tasks(id), predecessor TEXT NOT NULL REFERENCES tasks(id),
  PRIMARY KEY(task,predecessor), CHECK(task<>predecessor)
) STRICT;
CREATE TABLE attempts (
  id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), run TEXT NOT NULL REFERENCES runs(id),
  node TEXT NOT NULL REFERENCES nodes(id), profile TEXT NOT NULL, role TEXT NOT NULL,
  number INTEGER NOT NULL, epoch INTEGER NOT NULL, state TEXT NOT NULL,
  active INTEGER NOT NULL CHECK(active IN (0,1)), doc TEXT NOT NULL,
  UNIQUE(task,number)
) STRICT;
CREATE UNIQUE INDEX one_writer ON attempts(task) WHERE active=1 AND role='worker';
CREATE TABLE resources (
  attempt TEXT PRIMARY KEY REFERENCES attempts(id), node TEXT NOT NULL,
  worktree TEXT NOT NULL, pane TEXT NOT NULL, session TEXT NOT NULL,
  UNIQUE(node,worktree), UNIQUE(node,pane), UNIQUE(node,session)
) STRICT;
CREATE TABLE results (
  attempt TEXT PRIMARY KEY REFERENCES attempts(id), task TEXT NOT NULL REFERENCES tasks(id),
  contract_hash TEXT NOT NULL, commit_oid TEXT, doc TEXT NOT NULL
) STRICT;
CREATE TABLE operations (
  principal TEXT NOT NULL REFERENCES principals(id), id TEXT NOT NULL,
  request_hash TEXT NOT NULL, response TEXT NOT NULL, PRIMARY KEY(principal,id)
) STRICT;
CREATE TABLE journal (
  sequence INTEGER PRIMARY KEY, principal TEXT NOT NULL, operation TEXT NOT NULL,
  aggregate_id TEXT NOT NULL, kind TEXT NOT NULL, doc TEXT NOT NULL, created INTEGER NOT NULL
) STRICT;
CREATE TABLE outbox (
  id TEXT PRIMARY KEY, target TEXT NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending', tries INTEGER NOT NULL DEFAULT 0,
  available_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, last_error TEXT NOT NULL DEFAULT ''
) STRICT;
CREATE TABLE inbox (
  owner TEXT NOT NULL, author TEXT NOT NULL, id TEXT NOT NULL, content_hash TEXT NOT NULL,
  record TEXT NOT NULL, received INTEGER NOT NULL, applied INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(owner,author,id)
) STRICT;
CREATE TABLE memory_proposals (
  id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES runs(id), topic TEXT NOT NULL,
  author TEXT NOT NULL REFERENCES principals(id), source_attempt TEXT,
  text TEXT NOT NULL, evidence TEXT NOT NULL, status TEXT NOT NULL
) STRICT;
CREATE TABLE memory_revisions (
  run TEXT NOT NULL REFERENCES runs(id), topic TEXT NOT NULL, revision INTEGER NOT NULL,
  proposal TEXT REFERENCES memory_proposals(id), text TEXT NOT NULL,
  approved_by TEXT NOT NULL REFERENCES principals(id), withdrawn INTEGER NOT NULL,
  PRIMARY KEY(run,topic,revision)
) STRICT;
CREATE TABLE reviews (
  id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), candidate TEXT NOT NULL,
  principal TEXT NOT NULL REFERENCES principals(id), contract_hash TEXT NOT NULL, doc TEXT NOT NULL
) STRICT;
CREATE TABLE checks (
  id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), candidate TEXT NOT NULL,
  policy_hash TEXT NOT NULL, doc TEXT NOT NULL
) STRICT;
CREATE TABLE integrations (
  id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES runs(id), task TEXT NOT NULL REFERENCES tasks(id),
  base_oid TEXT NOT NULL, candidate TEXT NOT NULL, state TEXT NOT NULL, doc TEXT NOT NULL
) STRICT;
