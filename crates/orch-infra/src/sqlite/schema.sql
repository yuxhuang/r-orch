-- Initial 1.1 schema. Spec: docs/specs/2026-11-20-sqlite-storage.md, Schema for 1.1.

CREATE TABLE projects (
  project_id TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL, created_at INTEGER NOT NULL, revision INTEGER NOT NULL,
  PRIMARY KEY (project_id, id), CHECK (project_id = id)
) STRICT;

CREATE TABLE project_sources (
  project_id TEXT PRIMARY KEY,
  adapter TEXT NOT NULL CHECK (adapter IN ('local', 'managed')),
  location TEXT NOT NULL, location_pinned_at INTEGER,
  default_branch TEXT NOT NULL,
  integration_mode TEXT NOT NULL DEFAULT 'per_goal' CHECK (integration_mode IN ('per_goal', 'project')),
  project_integration_branch TEXT NOT NULL DEFAULT 'orch/integration',
  landing TEXT NOT NULL CHECK (landing IN ('merge', 'none')),
  repo_cap_bytes INTEGER NOT NULL DEFAULT 21474836480,
  checked_at INTEGER, checked_result TEXT, revision INTEGER NOT NULL
) STRICT;

CREATE TABLE project_config (
  project_id TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
  revision INTEGER NOT NULL, PRIMARY KEY (project_id, key)
) STRICT;

CREATE TABLE templates (
  project_id TEXT NOT NULL, name TEXT NOT NULL, body TEXT NOT NULL,
  revision INTEGER NOT NULL, PRIMARY KEY (project_id, name)
) STRICT;

CREATE TABLE goals (
  project_id TEXT NOT NULL, id TEXT NOT NULL, title TEXT NOT NULL, description TEXT NOT NULL,
  status TEXT NOT NULL, assignee_agent_id TEXT,
  position INTEGER NOT NULL,
  integration_branch TEXT,
  integration_head TEXT,
  published_at INTEGER, published_commit TEXT, published_by TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER, revision INTEGER NOT NULL,
  PRIMARY KEY (project_id, id)
) STRICT;

CREATE TABLE tasks (
  project_id TEXT NOT NULL, id TEXT NOT NULL, goal_id TEXT,
  title TEXT NOT NULL, description TEXT NOT NULL,
  status TEXT NOT NULL,

  priority INTEGER NOT NULL, assignee_agent_id TEXT,
  attempts INTEGER NOT NULL, max_attempts INTEGER NOT NULL,
  workspace_mode TEXT, awaiting_approval INTEGER NOT NULL DEFAULT 0, feedback TEXT,
  review TEXT,
  legacy_proof TEXT,
  outputs TEXT,
  position INTEGER NOT NULL,
  created_by_kind TEXT NOT NULL,
  created_by_run_id TEXT,
  accepted_by TEXT, accepted_at INTEGER,
  accepted_run_id TEXT,
  landing_state TEXT CHECK (landing_state IN ('pending', 'landed', 'conflict', 'skipped')),
  landed_commit TEXT, landed_into TEXT, landed_at INTEGER,
  landing_attempts INTEGER NOT NULL DEFAULT 0, landing_conflict_paths TEXT,
  attention_reason TEXT, attention_since INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, revision INTEGER NOT NULL,
  PRIMARY KEY (project_id, id),
  CHECK (status <> 'done' OR (landing_state IS NOT NULL AND landing_state IN ('landed', 'skipped'))),
  CHECK (landing_state IS NULL OR landing_state = 'skipped' OR accepted_run_id IS NOT NULL),
  CHECK (landing_state IS NULL OR landing_state <> 'landed' OR landed_commit IS NOT NULL)
) STRICT;

CREATE INDEX tasks_by_priority ON tasks (project_id, status, priority, id);

CREATE INDEX tasks_by_created ON tasks (project_id, status, created_at, id);

CREATE INDEX tasks_by_updated ON tasks (project_id, status, updated_at, id);

CREATE INDEX tasks_by_assignee ON tasks (project_id, assignee_agent_id, status, priority, id);

CREATE INDEX tasks_by_goal ON tasks (project_id, goal_id, status, priority, id);

CREATE INDEX tasks_all_by_created ON tasks (project_id, created_at, id);

CREATE INDEX tasks_all_by_updated ON tasks (project_id, updated_at, id);

CREATE INDEX tasks_landing_queue ON tasks (project_id, landing_state, updated_at)
  WHERE landing_state IN ('pending', 'conflict');

CREATE INDEX tasks_attention ON tasks (project_id, attention_since) WHERE attention_reason IS NOT NULL;

CREATE TABLE task_dependencies (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL, depends_on_id TEXT NOT NULL,
  PRIMARY KEY (project_id, task_id, depends_on_id)
) STRICT;

CREATE INDEX task_dependents ON task_dependencies (project_id, depends_on_id);

CREATE TABLE task_labels (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL, label TEXT NOT NULL,
  PRIMARY KEY (project_id, task_id, label)
) STRICT;

CREATE TABLE task_scopes (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL, pattern TEXT NOT NULL,
  PRIMARY KEY (project_id, task_id, pattern)
) STRICT;

CREATE TABLE blobs (
  project_id TEXT NOT NULL, sha256 TEXT NOT NULL, size INTEGER NOT NULL, storage_location TEXT NOT NULL, created_at INTEGER NOT NULL,
  unreferenced_since INTEGER,
  PRIMARY KEY (project_id, sha256)
) STRICT;
CREATE INDEX blobs_sweepable ON blobs (unreferenced_since) WHERE unreferenced_since IS NOT NULL;

CREATE TABLE task_attachments (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL, name TEXT NOT NULL, sha256 TEXT NOT NULL,
  PRIMARY KEY (project_id, task_id, name)
) STRICT;

CREATE TABLE agents (
  project_id TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL, adapter TEXT NOT NULL,
  role TEXT, config TEXT NOT NULL,
  autonomous INTEGER NOT NULL DEFAULT 0, position INTEGER NOT NULL,
  tasks_completed INTEGER NOT NULL DEFAULT 0, tasks_failed INTEGER NOT NULL DEFAULT 0,
  total_runs INTEGER NOT NULL DEFAULT 0, total_runtime_ms INTEGER NOT NULL DEFAULT 0, tokens_used INTEGER,
  last_run_finished_at INTEGER,
  last_error TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, revision INTEGER NOT NULL,
  PRIMARY KEY (project_id, id), UNIQUE (project_id, name)
) STRICT;

CREATE TABLE teams (
  project_id TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL, description TEXT,
  status TEXT NOT NULL, lead_agent_id TEXT NOT NULL, config TEXT NOT NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, revision INTEGER NOT NULL,
  PRIMARY KEY (project_id, id), UNIQUE (project_id, name)
) STRICT;

CREATE TABLE team_members (
  project_id TEXT NOT NULL, team_id TEXT NOT NULL, agent_id TEXT NOT NULL,
  role TEXT NOT NULL, joined_at INTEGER NOT NULL, PRIMARY KEY (project_id, team_id, agent_id)
) STRICT;

CREATE TABLE team_tasks (
  project_id TEXT NOT NULL, team_id TEXT NOT NULL, task_id TEXT NOT NULL,
  PRIMARY KEY (project_id, team_id, task_id)
) STRICT;

CREATE TABLE runs (
  project_id TEXT NOT NULL, id TEXT NOT NULL, goal_id TEXT, task_id TEXT NOT NULL,
  agent_id TEXT NOT NULL, worker_id TEXT,
  run_no INTEGER NOT NULL,
  attempt_no INTEGER,
  counted INTEGER NOT NULL,
  epoch INTEGER NOT NULL, branch TEXT,
  base_sha TEXT,
  head_sha TEXT, head_verified_at INTEGER,
  push_window_until INTEGER,
  status TEXT NOT NULL, started_at INTEGER NOT NULL, finished_at INTEGER,
  prompt_sha256 TEXT,
  error TEXT, error_kind TEXT,
  tokens_input INTEGER, tokens_output INTEGER, tokens_reasoning INTEGER,
  tokens_cache_read INTEGER, tokens_cache_write INTEGER,
  model TEXT, adapter_version TEXT,
  events_pruned_at INTEGER, events_pruned_count INTEGER,
  local TEXT, revision INTEGER NOT NULL,
  PRIMARY KEY (project_id, id),
  CHECK ((counted = 1 AND attempt_no IS NOT NULL) OR (counted = 0 AND attempt_no IS NULL))
) STRICT;

CREATE UNIQUE INDEX runs_epoch ON runs (project_id, task_id, epoch);

CREATE UNIQUE INDEX runs_seq ON runs (project_id, task_id, run_no);

CREATE UNIQUE INDEX runs_attempt ON runs (project_id, task_id, attempt_no);

CREATE INDEX runs_by_agent ON runs (project_id, agent_id, started_at DESC);

CREATE TABLE task_epochs (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL,
  next_epoch INTEGER NOT NULL, lost_streak INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (project_id, task_id)
) STRICT;

CREATE TABLE leases (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL, run_id TEXT NOT NULL, agent_id TEXT NOT NULL,
  worker_id TEXT NOT NULL, epoch INTEGER NOT NULL, lineage TEXT NOT NULL, branch TEXT,
  base_sha TEXT, fetch_ref TEXT,
  state TEXT NOT NULL CHECK (state IN ('offered', 'active')),
  granted_at INTEGER NOT NULL, offered_at INTEGER,
  renewed_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  PRIMARY KEY (project_id, task_id)
) STRICT;

CREATE UNIQUE INDEX one_active_run_per_agent ON leases (project_id, agent_id) WHERE state = 'active';

CREATE INDEX leases_expiring ON leases (expires_at);

CREATE INDEX leases_by_worker ON leases (worker_id);

CREATE TABLE retry_queue (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL, attempt INTEGER NOT NULL,
  due_at INTEGER NOT NULL, error TEXT, PRIMARY KEY (project_id, task_id)
) STRICT;

CREATE INDEX retries_due ON retry_queue (due_at);

CREATE TABLE intents (
  project_id TEXT NOT NULL DEFAULT '', id INTEGER NOT NULL, worker_id TEXT, run_id TEXT,
  kind TEXT NOT NULL, payload TEXT NOT NULL, created_at INTEGER NOT NULL, done_at INTEGER,
  PRIMARY KEY (project_id, id)
) STRICT;
CREATE INDEX intents_pending ON intents (id) WHERE done_at IS NULL;

CREATE TABLE request_keys (
  identity_id TEXT NOT NULL, project_id TEXT NOT NULL DEFAULT '',
  run_id TEXT NOT NULL DEFAULT '', epoch INTEGER NOT NULL DEFAULT 0,
  key TEXT NOT NULL, op TEXT NOT NULL, req_sha256 TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('in_flight', 'done')),
  result TEXT, result_truncated INTEGER NOT NULL DEFAULT 0,
  commit_seq INTEGER, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  PRIMARY KEY (identity_id, project_id, run_id, epoch, key)
) STRICT, WITHOUT ROWID;

CREATE INDEX request_keys_expiring ON request_keys (expires_at);

CREATE TABLE results (
  project_id TEXT NOT NULL, run_id TEXT NOT NULL, goal_id TEXT, task_id TEXT NOT NULL,
  claimed_verdict TEXT,
  verdict TEXT,
  summary TEXT, base_sha TEXT, head_sha TEXT,
  tests_passed INTEGER, tests_failed INTEGER, created_at INTEGER NOT NULL,
  PRIMARY KEY (project_id, run_id)
) STRICT;

CREATE INDEX results_by_goal ON results (project_id, goal_id, created_at DESC);

CREATE TABLE run_events (
  project_id TEXT NOT NULL, run_id TEXT NOT NULL, seq INTEGER NOT NULL,
  at INTEGER NOT NULL, type TEXT NOT NULL, payload TEXT NOT NULL,
  PRIMARY KEY (project_id, run_id, seq)
) STRICT, WITHOUT ROWID;

CREATE TABLE run_event_gaps (
  project_id TEXT NOT NULL, run_id TEXT NOT NULL, after_seq INTEGER NOT NULL, dropped INTEGER NOT NULL,
  PRIMARY KEY (project_id, run_id, after_seq)
) STRICT;

CREATE TABLE messages (
  project_id TEXT NOT NULL, id TEXT NOT NULL, channel TEXT NOT NULL,
  from_agent_id TEXT NOT NULL, to_agent_id TEXT, team_id TEXT, reply_to TEXT,
  subject TEXT NOT NULL, body TEXT NOT NULL, status TEXT NOT NULL, position INTEGER NOT NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER, delivered_at INTEGER,
  PRIMARY KEY (project_id, id)
) STRICT;

CREATE INDEX messages_pending ON messages (project_id, to_agent_id) WHERE status = 'pending';

CREATE TABLE context_entries (
  project_id TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
  set_by_kind TEXT NOT NULL, set_by_agent_id TEXT, set_by_run_id TEXT,
  updated_at INTEGER NOT NULL, expires_at INTEGER, revision INTEGER NOT NULL,
  PRIMARY KEY (project_id, key)
) STRICT;

CREATE TABLE daemon_config (key TEXT PRIMARY KEY, value TEXT NOT NULL, revision INTEGER NOT NULL) STRICT;

CREATE TABLE identities (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('owner', 'worker', 'service')),
  name TEXT NOT NULL UNIQUE, enabled INTEGER NOT NULL DEFAULT 1,
  confirmed_at INTEGER,
  created_at INTEGER NOT NULL, disabled_at INTEGER, revision INTEGER NOT NULL
) STRICT;

CREATE TABLE identity_tokens (
  id TEXT PRIMARY KEY, identity_id TEXT NOT NULL, token_sha256 TEXT NOT NULL UNIQUE,
  allows_destructive INTEGER NOT NULL DEFAULT 0, label TEXT,
  created_at INTEGER NOT NULL, expires_at INTEGER, revoked_at INTEGER, last_used_at INTEGER
) STRICT;

CREATE INDEX identity_tokens_live ON identity_tokens (identity_id) WHERE revoked_at IS NULL;

CREATE TABLE attention (
  id TEXT NOT NULL,
  project_id TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL,
  subject_kind TEXT NOT NULL,
  subject_id TEXT NOT NULL,
  severity TEXT NOT NULL CHECK (severity IN ('blocking', 'warning')),
  title TEXT NOT NULL, detail TEXT,
  author_kind TEXT NOT NULL DEFAULT 'daemon' CHECK (author_kind IN ('daemon', 'agent', 'owner')),
  author_run_id TEXT,
  opened_at INTEGER NOT NULL,
  acked_at INTEGER, acked_by TEXT,
  resolved_at INTEGER, resolved_by TEXT, resolved_reason TEXT,
  revision INTEGER NOT NULL,
  PRIMARY KEY (project_id, id)
) STRICT;
CREATE UNIQUE INDEX attention_one_per_subject
  ON attention (project_id, kind, subject_id) WHERE resolved_at IS NULL;
CREATE INDEX attention_open ON attention (project_id, severity, opened_at) WHERE resolved_at IS NULL;
CREATE INDEX attention_history ON attention (project_id, resolved_at);

CREATE TABLE counters (
  project_id TEXT NOT NULL, name TEXT NOT NULL, value INTEGER NOT NULL, PRIMARY KEY (project_id, name)
) STRICT;

CREATE TABLE import_manifest (
  project_id TEXT NOT NULL, entity TEXT NOT NULL, entity_id TEXT NOT NULL,
  source_sha256 TEXT NOT NULL, imported_at INTEGER NOT NULL,
  PRIMARY KEY (project_id, entity, entity_id)
) STRICT;

CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL) STRICT;

CREATE UNIQUE INDEX tasks_id_ascii_case ON tasks(project_id, id COLLATE NOCASE);

INSERT INTO daemon_config(key,value,revision) VALUES('lineage',lower(hex(randomblob(16))),1);
INSERT INTO identities(id,kind,name,created_at,revision) VALUES
 ('local_owner','owner','Local owner',CAST(unixepoch('subsec')*1000 AS INTEGER),1),
 ('local_worker','worker','Local worker',CAST(unixepoch('subsec')*1000 AS INTEGER),1);
