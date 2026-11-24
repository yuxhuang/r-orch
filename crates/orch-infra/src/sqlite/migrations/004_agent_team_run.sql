-- Entity format v1. This migration is guarded by the registry's atomic
-- populated-predecessor refusal. It never invents a lossy backfill.
CREATE TABLE entity_details (
  project_id TEXT NOT NULL,
  family TEXT NOT NULL CHECK (family IN ('agent','team','run','context','config','template','goal')),
  key_form INTEGER NOT NULL CHECK (key_form IN (0,1)),
  entity_key TEXT NOT NULL,
  format INTEGER NOT NULL CHECK (format >= 1),
  detail TEXT, detail_bytes INTEGER NOT NULL CHECK (detail_bytes >= 0),
  detail_sha256 TEXT NOT NULL CHECK (length(detail_sha256) = 64 AND detail_sha256 NOT GLOB '*[^0-9a-f]*'),
  PRIMARY KEY (project_id,family,key_form,entity_key),
  CHECK ((detail IS NULL AND detail_bytes > 65536)
      OR (detail IS NOT NULL AND detail_bytes <= 65536 AND length(CAST(detail AS BLOB)) = detail_bytes)),
  CHECK ((key_form = 0 AND length(CAST(entity_key AS BLOB)) <= 65536)
      OR (key_form = 1 AND length(entity_key) = 64 AND entity_key NOT GLOB '*[^0-9a-f]*'))
) STRICT;
CREATE TABLE entity_blob_refs (
  project_id TEXT NOT NULL,
  family TEXT NOT NULL CHECK (family IN ('agent','team','run','context','config','template','goal')),
  key_form INTEGER NOT NULL CHECK (key_form IN (0,1)),
  entity_key TEXT NOT NULL, locator TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
  size INTEGER NOT NULL CHECK (size BETWEEN 0 AND 67108864 AND (size > 0 OR (family = 'run' AND locator = '/prompt'))),
  PRIMARY KEY (project_id,family,key_form,entity_key,locator,ordinal),
  CHECK ((key_form = 0 AND length(CAST(entity_key AS BLOB)) <= 65536)
      OR (key_form = 1 AND length(entity_key) = 64 AND entity_key NOT GLOB '*[^0-9a-f]*'))
) STRICT, WITHOUT ROWID;
CREATE INDEX entity_blob_refs_by_hash ON entity_blob_refs (project_id,sha256);

-- No inbound SQL foreign keys reference these predecessor tables. Their
-- contents must be empty: the hook checks before any DROP or ledger change.
DROP TABLE team_members;
DROP TABLE team_tasks;
DROP TABLE agents;
DROP TABLE teams;
DROP TABLE runs;

CREATE TABLE agents (
  id TEXT NOT NULL,
  id_form INTEGER NOT NULL DEFAULT 0 CHECK (id_form IN (0,1)),
  id_sha256 TEXT, id_bytes INTEGER,
  name TEXT NOT NULL,
  name_form INTEGER NOT NULL DEFAULT 0 CHECK (name_form IN (0,1)),
  name_sha256 TEXT, name_bytes INTEGER,
  adapter TEXT NOT NULL,
  adapter_form INTEGER NOT NULL DEFAULT 0 CHECK (adapter_form IN (0,1)),
  adapter_sha256 TEXT, adapter_bytes INTEGER,
  role TEXT,
  role_form INTEGER NOT NULL DEFAULT 0 CHECK (role_form IN (0,1)),
  role_sha256 TEXT, role_bytes INTEGER,
  current_task TEXT,
  current_task_form INTEGER NOT NULL DEFAULT 0 CHECK (current_task_form IN (0,1)),
  current_task_sha256 TEXT, current_task_bytes INTEGER,
  last_error TEXT,
  last_error_form INTEGER NOT NULL DEFAULT 0 CHECK (last_error_form IN (0,1)),
  last_error_sha256 TEXT, last_error_bytes INTEGER,
  project_id TEXT NOT NULL,
  config TEXT NOT NULL CHECK (config = '{}'),
  status TEXT NOT NULL DEFAULT 'idle',
  autonomous INTEGER CHECK (autonomous IN (0,1)),
  position INTEGER NOT NULL CHECK (position >= 0),
  tasks_completed INTEGER CHECK (tasks_completed >= 0),
  tasks_failed INTEGER CHECK (tasks_failed >= 0),
  total_runs INTEGER CHECK (total_runs >= 0),
  total_runtime_ms INTEGER CHECK (total_runtime_ms >= 0),
  tokens_used INTEGER CHECK (tokens_used >= 0),
  last_run_finished_at INTEGER, created_at INTEGER, updated_at INTEGER,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_id,id_form,id),
  CHECK ((id_form = 0 AND id IS NOT NULL AND length(CAST(id AS BLOB)) <= 65536 AND id_sha256 IS NULL AND id_bytes IS NULL)
      OR (id_form = 1 AND id IS NOT NULL AND id_sha256 IS NOT NULL AND id_bytes IS NOT NULL
          AND id = id_sha256 AND length(id_sha256) = 64 AND id_sha256 NOT GLOB '*[^0-9a-f]*' AND id_bytes > 65536)),
  CHECK ((name_form = 0 AND name IS NOT NULL AND length(CAST(name AS BLOB)) <= 65536 AND name_sha256 IS NULL AND name_bytes IS NULL)
      OR (name_form = 1 AND name IS NOT NULL AND name_sha256 IS NOT NULL AND name_bytes IS NOT NULL
          AND name = '' AND length(name_sha256) = 64 AND name_sha256 NOT GLOB '*[^0-9a-f]*' AND name_bytes > 65536)),
  CHECK ((adapter_form = 0 AND adapter IS NOT NULL AND length(CAST(adapter AS BLOB)) <= 65536 AND adapter_sha256 IS NULL AND adapter_bytes IS NULL)
      OR (adapter_form = 1 AND adapter IS NOT NULL AND adapter_sha256 IS NOT NULL AND adapter_bytes IS NOT NULL
          AND adapter = '' AND length(adapter_sha256) = 64 AND adapter_sha256 NOT GLOB '*[^0-9a-f]*' AND adapter_bytes > 65536)),
  CHECK ((role_form = 0 AND (role IS NULL OR (role IS NOT NULL AND length(CAST(role AS BLOB)) <= 65536)) AND role_sha256 IS NULL AND role_bytes IS NULL)
      OR (role_form = 1 AND role IS NOT NULL AND role_sha256 IS NOT NULL AND role_bytes IS NOT NULL
          AND role = '' AND length(role_sha256) = 64 AND role_sha256 NOT GLOB '*[^0-9a-f]*' AND role_bytes > 65536)),
  CHECK ((current_task_form = 0 AND (current_task IS NULL OR (current_task IS NOT NULL AND length(CAST(current_task AS BLOB)) <= 65536)) AND current_task_sha256 IS NULL AND current_task_bytes IS NULL)
      OR (current_task_form = 1 AND current_task IS NOT NULL AND current_task_sha256 IS NOT NULL AND current_task_bytes IS NOT NULL
          AND current_task = '' AND length(current_task_sha256) = 64 AND current_task_sha256 NOT GLOB '*[^0-9a-f]*' AND current_task_bytes > 65536)),
  CHECK ((last_error_form = 0 AND (last_error IS NULL OR (last_error IS NOT NULL AND length(CAST(last_error AS BLOB)) <= 65536)) AND last_error_sha256 IS NULL AND last_error_bytes IS NULL)
      OR (last_error_form = 1 AND last_error IS NOT NULL AND last_error_sha256 IS NOT NULL AND last_error_bytes IS NOT NULL
          AND last_error = '' AND length(last_error_sha256) = 64 AND last_error_sha256 NOT GLOB '*[^0-9a-f]*' AND last_error_bytes > 65536))
) STRICT;

CREATE TABLE teams (
  id TEXT NOT NULL,
  id_form INTEGER NOT NULL DEFAULT 0 CHECK (id_form IN (0,1)),
  id_sha256 TEXT, id_bytes INTEGER,
  name TEXT NOT NULL,
  name_form INTEGER NOT NULL DEFAULT 0 CHECK (name_form IN (0,1)),
  name_sha256 TEXT, name_bytes INTEGER,
  description TEXT,
  description_form INTEGER NOT NULL DEFAULT 0 CHECK (description_form IN (0,1)),
  description_sha256 TEXT, description_bytes INTEGER,
  lead_agent_id TEXT NOT NULL,
  lead_agent_id_form INTEGER NOT NULL DEFAULT 0 CHECK (lead_agent_id_form IN (0,1)),
  lead_agent_id_sha256 TEXT, lead_agent_id_bytes INTEGER,
  created_at_text TEXT NOT NULL,
  created_at_text_form INTEGER NOT NULL DEFAULT 0 CHECK (created_at_text_form IN (0,1)),
  created_at_text_sha256 TEXT, created_at_text_bytes INTEGER,
  project_id TEXT NOT NULL,
  status TEXT NOT NULL, config TEXT NOT NULL,
  created_at INTEGER, updated_at INTEGER,
  position INTEGER NOT NULL CHECK (position >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_id,id_form,id),
  CHECK ((id_form = 0 AND id IS NOT NULL AND length(CAST(id AS BLOB)) <= 65536 AND id_sha256 IS NULL AND id_bytes IS NULL)
      OR (id_form = 1 AND id IS NOT NULL AND id_sha256 IS NOT NULL AND id_bytes IS NOT NULL
          AND id = id_sha256 AND length(id_sha256) = 64 AND id_sha256 NOT GLOB '*[^0-9a-f]*' AND id_bytes > 65536)),
  CHECK ((name_form = 0 AND name IS NOT NULL AND length(CAST(name AS BLOB)) <= 65536 AND name_sha256 IS NULL AND name_bytes IS NULL)
      OR (name_form = 1 AND name IS NOT NULL AND name_sha256 IS NOT NULL AND name_bytes IS NOT NULL
          AND name = '' AND length(name_sha256) = 64 AND name_sha256 NOT GLOB '*[^0-9a-f]*' AND name_bytes > 65536)),
  CHECK ((description_form = 0 AND (description IS NULL OR (description IS NOT NULL AND length(CAST(description AS BLOB)) <= 65536)) AND description_sha256 IS NULL AND description_bytes IS NULL)
      OR (description_form = 1 AND description IS NOT NULL AND description_sha256 IS NOT NULL AND description_bytes IS NOT NULL
          AND description = '' AND length(description_sha256) = 64 AND description_sha256 NOT GLOB '*[^0-9a-f]*' AND description_bytes > 65536)),
  CHECK ((lead_agent_id_form = 0 AND lead_agent_id IS NOT NULL AND length(CAST(lead_agent_id AS BLOB)) <= 65536 AND lead_agent_id_sha256 IS NULL AND lead_agent_id_bytes IS NULL)
      OR (lead_agent_id_form = 1 AND lead_agent_id IS NOT NULL AND lead_agent_id_sha256 IS NOT NULL AND lead_agent_id_bytes IS NOT NULL
          AND lead_agent_id = '' AND length(lead_agent_id_sha256) = 64 AND lead_agent_id_sha256 NOT GLOB '*[^0-9a-f]*' AND lead_agent_id_bytes > 65536)),
  CHECK ((created_at_text_form = 0 AND created_at_text IS NOT NULL AND length(CAST(created_at_text AS BLOB)) <= 65536 AND created_at_text_sha256 IS NULL AND created_at_text_bytes IS NULL)
      OR (created_at_text_form = 1 AND created_at_text IS NOT NULL AND created_at_text_sha256 IS NOT NULL AND created_at_text_bytes IS NOT NULL
          AND created_at_text = '' AND length(created_at_text_sha256) = 64 AND created_at_text_sha256 NOT GLOB '*[^0-9a-f]*' AND created_at_text_bytes > 65536))
) STRICT;

CREATE TABLE team_members (
  team_id TEXT NOT NULL,
  team_id_form INTEGER NOT NULL DEFAULT 0 CHECK (team_id_form IN (0,1)),
  team_id_sha256 TEXT, team_id_bytes INTEGER,
  role TEXT NOT NULL,
  role_form INTEGER NOT NULL DEFAULT 0 CHECK (role_form IN (0,1)),
  role_sha256 TEXT, role_bytes INTEGER,
  joined_at_text TEXT NOT NULL,
  joined_at_text_form INTEGER NOT NULL DEFAULT 0 CHECK (joined_at_text_form IN (0,1)),
  joined_at_text_sha256 TEXT, joined_at_text_bytes INTEGER,
  project_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  agent_id TEXT CHECK (length(CAST(agent_id AS BLOB)) <= 65536),
  joined_at INTEGER,
  PRIMARY KEY (project_id,team_id_form,team_id,ordinal),
  CHECK ((team_id_form = 0 AND team_id IS NOT NULL AND length(CAST(team_id AS BLOB)) <= 65536 AND team_id_sha256 IS NULL AND team_id_bytes IS NULL)
      OR (team_id_form = 1 AND team_id IS NOT NULL AND team_id_sha256 IS NOT NULL AND team_id_bytes IS NOT NULL
          AND team_id = team_id_sha256 AND length(team_id_sha256) = 64 AND team_id_sha256 NOT GLOB '*[^0-9a-f]*' AND team_id_bytes > 65536)),
  CHECK ((role_form = 0 AND role IS NOT NULL AND length(CAST(role AS BLOB)) <= 65536 AND role_sha256 IS NULL AND role_bytes IS NULL)
      OR (role_form = 1 AND role IS NOT NULL AND role_sha256 IS NOT NULL AND role_bytes IS NOT NULL
          AND role = '' AND length(role_sha256) = 64 AND role_sha256 NOT GLOB '*[^0-9a-f]*' AND role_bytes > 65536)),
  CHECK ((joined_at_text_form = 0 AND joined_at_text IS NOT NULL AND length(CAST(joined_at_text AS BLOB)) <= 65536 AND joined_at_text_sha256 IS NULL AND joined_at_text_bytes IS NULL)
      OR (joined_at_text_form = 1 AND joined_at_text IS NOT NULL AND joined_at_text_sha256 IS NOT NULL AND joined_at_text_bytes IS NOT NULL
          AND joined_at_text = '' AND length(joined_at_text_sha256) = 64 AND joined_at_text_sha256 NOT GLOB '*[^0-9a-f]*' AND joined_at_text_bytes > 65536))
) STRICT;

CREATE TABLE team_tasks (
  team_id TEXT NOT NULL,
  team_id_form INTEGER NOT NULL DEFAULT 0 CHECK (team_id_form IN (0,1)),
  team_id_sha256 TEXT, team_id_bytes INTEGER,
  project_id TEXT NOT NULL,
  task_id TEXT NOT NULL CHECK (length(CAST(task_id AS BLOB)) <= 65536),
  PRIMARY KEY (project_id,team_id_form,team_id,task_id),
  CHECK ((team_id_form = 0 AND team_id IS NOT NULL AND length(CAST(team_id AS BLOB)) <= 65536 AND team_id_sha256 IS NULL AND team_id_bytes IS NULL)
      OR (team_id_form = 1 AND team_id IS NOT NULL AND team_id_sha256 IS NOT NULL AND team_id_bytes IS NOT NULL
          AND team_id = team_id_sha256 AND length(team_id_sha256) = 64 AND team_id_sha256 NOT GLOB '*[^0-9a-f]*' AND team_id_bytes > 65536))
) STRICT;

CREATE TABLE runs (
  id TEXT NOT NULL,
  id_form INTEGER NOT NULL DEFAULT 0 CHECK (id_form IN (0,1)),
  id_sha256 TEXT, id_bytes INTEGER,
  task_id TEXT NOT NULL,
  task_id_form INTEGER NOT NULL DEFAULT 0 CHECK (task_id_form IN (0,1)),
  task_id_sha256 TEXT, task_id_bytes INTEGER,
  agent_id TEXT NOT NULL,
  agent_id_form INTEGER NOT NULL DEFAULT 0 CHECK (agent_id_form IN (0,1)),
  agent_id_sha256 TEXT, agent_id_bytes INTEGER,
  started_at_text TEXT NOT NULL DEFAULT '',
  started_at_text_form INTEGER NOT NULL DEFAULT 0 CHECK (started_at_text_form IN (0,1)),
  started_at_text_sha256 TEXT, started_at_text_bytes INTEGER,
  error TEXT,
  error_form INTEGER NOT NULL DEFAULT 0 CHECK (error_form IN (0,1)),
  error_sha256 TEXT, error_bytes INTEGER,
  project_id TEXT NOT NULL,
  goal_id TEXT, worker_id TEXT,
  run_no INTEGER CHECK (run_no >= 0),
  attempt_no INTEGER CHECK (attempt_no >= 0),
  counted INTEGER, epoch INTEGER CHECK (epoch >= 0), branch TEXT,
  base_sha TEXT, head_sha TEXT, head_verified_at INTEGER, push_window_until INTEGER,
  status TEXT NOT NULL, started_at INTEGER, finished_at INTEGER,
  attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt BETWEEN 0 AND 4294967295),
  position INTEGER NOT NULL DEFAULT 0 CHECK (position >= 0),
  prompt_sha256 TEXT, error_kind TEXT,
  tokens_input INTEGER CHECK (tokens_input >= 0),
  tokens_output INTEGER CHECK (tokens_output >= 0),
  tokens_reasoning INTEGER CHECK (tokens_reasoning >= 0),
  tokens_cache_read INTEGER CHECK (tokens_cache_read >= 0),
  tokens_cache_write INTEGER CHECK (tokens_cache_write >= 0),
  tokens_total INTEGER CHECK (tokens_total >= 0),
  model TEXT, adapter_version TEXT, events_pruned_at INTEGER, events_pruned_count INTEGER,
  local TEXT, revision INTEGER NOT NULL CHECK (revision >= 1),
  CHECK ((run_no IS NULL AND counted IS NULL AND epoch IS NULL AND attempt_no IS NULL)
      OR (run_no IS NOT NULL AND epoch IS NOT NULL AND counted IS NOT NULL
          AND ((counted = 1 AND attempt_no IS NOT NULL) OR (counted = 0 AND attempt_no IS NULL)))),
  PRIMARY KEY (project_id,id_form,id),
  CHECK ((id_form = 0 AND id IS NOT NULL AND length(CAST(id AS BLOB)) <= 65536 AND id_sha256 IS NULL AND id_bytes IS NULL)
      OR (id_form = 1 AND id IS NOT NULL AND id_sha256 IS NOT NULL AND id_bytes IS NOT NULL
          AND id = id_sha256 AND length(id_sha256) = 64 AND id_sha256 NOT GLOB '*[^0-9a-f]*' AND id_bytes > 65536)),
  CHECK ((task_id_form = 0 AND task_id IS NOT NULL AND length(CAST(task_id AS BLOB)) <= 65536 AND task_id_sha256 IS NULL AND task_id_bytes IS NULL)
      OR (task_id_form = 1 AND task_id IS NOT NULL AND task_id_sha256 IS NOT NULL AND task_id_bytes IS NOT NULL
          AND task_id = task_id_sha256 AND length(task_id_sha256) = 64 AND task_id_sha256 NOT GLOB '*[^0-9a-f]*' AND task_id_bytes > 65536)),
  CHECK ((agent_id_form = 0 AND agent_id IS NOT NULL AND length(CAST(agent_id AS BLOB)) <= 65536 AND agent_id_sha256 IS NULL AND agent_id_bytes IS NULL)
      OR (agent_id_form = 1 AND agent_id IS NOT NULL AND agent_id_sha256 IS NOT NULL AND agent_id_bytes IS NOT NULL
          AND agent_id = agent_id_sha256 AND length(agent_id_sha256) = 64 AND agent_id_sha256 NOT GLOB '*[^0-9a-f]*' AND agent_id_bytes > 65536)),
  CHECK ((started_at_text_form = 0 AND started_at_text IS NOT NULL AND length(CAST(started_at_text AS BLOB)) <= 65536 AND started_at_text_sha256 IS NULL AND started_at_text_bytes IS NULL)
      OR (started_at_text_form = 1 AND started_at_text IS NOT NULL AND started_at_text_sha256 IS NOT NULL AND started_at_text_bytes IS NOT NULL
          AND started_at_text = '' AND length(started_at_text_sha256) = 64 AND started_at_text_sha256 NOT GLOB '*[^0-9a-f]*' AND started_at_text_bytes > 65536)),
  CHECK ((error_form = 0 AND (error IS NULL OR (error IS NOT NULL AND length(CAST(error AS BLOB)) <= 65536)) AND error_sha256 IS NULL AND error_bytes IS NULL)
      OR (error_form = 1 AND error IS NOT NULL AND error_sha256 IS NOT NULL AND error_bytes IS NOT NULL
          AND error = '' AND length(error_sha256) = 64 AND error_sha256 NOT GLOB '*[^0-9a-f]*' AND error_bytes > 65536))
) STRICT;

CREATE INDEX agents_by_name ON agents (project_id,name_form,name);
CREATE INDEX teams_by_name ON teams (project_id,name_form,name);
CREATE INDEX team_members_by_agent ON team_members (project_id,agent_id,team_id_form,team_id) WHERE agent_id IS NOT NULL;
-- Form participates in claim identity: a literal SHA-looking task ID cannot
-- alias a different task whose oversized ID hashes to those bytes.
CREATE UNIQUE INDEX runs_epoch ON runs (project_id,task_id_form,task_id,epoch);
CREATE UNIQUE INDEX runs_seq ON runs (project_id,task_id_form,task_id,run_no);
CREATE UNIQUE INDEX runs_attempt ON runs (project_id,task_id_form,task_id,attempt_no);
CREATE INDEX runs_by_agent ON runs (project_id,agent_id_form,agent_id,started_at DESC);
CREATE INDEX runs_by_task_attempt ON runs (project_id,task_id_form,task_id,attempt DESC,id_form,id);
