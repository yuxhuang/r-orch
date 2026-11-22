CREATE TABLE daemon (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  lineage TEXT NOT NULL,
  session TEXT NOT NULL
) STRICT;
INSERT INTO daemon VALUES (1, 'lineage-1', 'session-1');
CREATE TABLE tasks (
  project TEXT NOT NULL, task TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('ready', 'in_progress', 'done')),
  attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  PRIMARY KEY (project, task)
) STRICT;
-- Intentionally no task FK: deleting/recreating a task must retain its counter.
CREATE TABLE task_epochs (
  project TEXT NOT NULL, task TEXT NOT NULL, epoch INTEGER NOT NULL CHECK (epoch >= 0),
  PRIMARY KEY (project, task)
) STRICT;
CREATE TRIGGER retain_epoch BEFORE DELETE ON task_epochs
BEGIN SELECT RAISE(ABORT, 'epoch counters are permanent'); END;
CREATE TRIGGER monotonic_epoch BEFORE UPDATE OF epoch ON task_epochs
WHEN NEW.epoch < OLD.epoch
BEGIN SELECT RAISE(ABORT, 'epoch cannot decrease'); END;
CREATE TABLE runs (
  project TEXT NOT NULL, run TEXT NOT NULL, task TEXT NOT NULL,
  agent TEXT NOT NULL, lineage TEXT NOT NULL, epoch INTEGER NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('running', 'lost', 'succeeded', 'failed')),
  PRIMARY KEY (project, run), UNIQUE (project, task, lineage, epoch),
  FOREIGN KEY (project, task) REFERENCES tasks (project, task)
) STRICT;
CREATE TABLE leases (
  project TEXT NOT NULL, task TEXT NOT NULL, agent TEXT NOT NULL,
  run TEXT NOT NULL, lineage TEXT NOT NULL, epoch INTEGER NOT NULL,
  session TEXT NOT NULL, deadline INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active', 'recovering')),
  PRIMARY KEY (project, task),
  FOREIGN KEY (project, run) REFERENCES runs (project, run) DEFERRABLE INITIALLY DEFERRED
) STRICT;
-- Recovery reserves an agent too: it must not acquire another task during grace.
CREATE UNIQUE INDEX one_agent ON leases (project, agent)
WHERE state IN ('active', 'recovering');
CREATE TABLE retries (
  project TEXT NOT NULL, task TEXT NOT NULL,
  PRIMARY KEY (project, task),
  FOREIGN KEY (project, task) REFERENCES tasks (project, task)
) STRICT;
CREATE TABLE intents (
  project TEXT NOT NULL, run TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('pending', 'cancelled')),
  PRIMARY KEY (project, run),
  FOREIGN KEY (project, run) REFERENCES runs (project, run)
) STRICT;
