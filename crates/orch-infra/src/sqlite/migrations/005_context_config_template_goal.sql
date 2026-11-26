-- Entity format v1 for Context, Config, Template and Goal. This migration is
-- guarded by the registry's atomic populated-predecessor refusal. It never
-- invents a lossy backfill and creates no table: it rebuilds four empty ones.
DROP TABLE context_entries;
DROP TABLE project_config;
DROP TABLE templates;
DROP TABLE goals;


CREATE TABLE context_entries (
  project_id TEXT NOT NULL,
  key TEXT NOT NULL,
  key_form INTEGER NOT NULL DEFAULT 0 CHECK (key_form IN (0,1)),
  key_sha256 TEXT, key_bytes INTEGER,
  value TEXT NOT NULL,
  value_form INTEGER NOT NULL DEFAULT 0 CHECK (value_form IN (0,1)),
  value_sha256 TEXT, value_bytes INTEGER,
  set_by_kind TEXT NOT NULL,
  set_by_identity TEXT CHECK (set_by_identity IS NULL OR length(CAST(set_by_identity AS BLOB)) <= 65536),
  set_by_agent_id TEXT CHECK (set_by_agent_id IS NULL OR length(CAST(set_by_agent_id AS BLOB)) <= 65536),
  set_by_run_id TEXT CHECK (set_by_run_id IS NULL OR length(CAST(set_by_run_id AS BLOB)) <= 65536),
  created_at INTEGER, updated_at INTEGER, expires_at INTEGER,
  ttl_ms INTEGER CHECK (ttl_ms IS NULL OR ttl_ms >= 0),
  position INTEGER NOT NULL CHECK (position >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_id,key_form,key),
  CHECK ((key_form = 0 AND key IS NOT NULL AND length(CAST(key AS BLOB)) <= 65536 AND key_sha256 IS NULL AND key_bytes IS NULL)
      OR (key_form = 1 AND key IS NOT NULL AND key_sha256 IS NOT NULL AND key_bytes IS NOT NULL
          AND key = key_sha256 AND length(key_sha256) = 64 AND length(CAST(key_sha256 AS BLOB)) = 64 AND key_sha256 NOT GLOB '*[^0-9a-f]*' AND key_bytes > 65536)),
  CHECK ((value_form = 0 AND value IS NOT NULL AND length(CAST(value AS BLOB)) <= 65536 AND value_sha256 IS NULL AND value_bytes IS NULL)
      OR (value_form = 1 AND value IS NOT NULL AND value_sha256 IS NOT NULL AND value_bytes IS NOT NULL
          AND value = '' AND length(value_sha256) = 64 AND length(CAST(value_sha256 AS BLOB)) = 64 AND value_sha256 NOT GLOB '*[^0-9a-f]*' AND value_bytes > 65536))
) STRICT;

CREATE TABLE project_config (
  project_id TEXT NOT NULL,
  key TEXT NOT NULL,
  key_form INTEGER NOT NULL DEFAULT 0 CHECK (key_form IN (0,1)),
  key_sha256 TEXT, key_bytes INTEGER,
  value TEXT NOT NULL,
  value_form INTEGER NOT NULL DEFAULT 0 CHECK (value_form IN (0,1)),
  value_sha256 TEXT, value_bytes INTEGER,
  position INTEGER NOT NULL CHECK (position >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_id,key_form,key),
  CHECK ((key_form = 0 AND key IS NOT NULL AND length(CAST(key AS BLOB)) <= 65536 AND key_sha256 IS NULL AND key_bytes IS NULL)
      OR (key_form = 1 AND key IS NOT NULL AND key_sha256 IS NOT NULL AND key_bytes IS NOT NULL
          AND key = key_sha256 AND length(key_sha256) = 64 AND length(CAST(key_sha256 AS BLOB)) = 64 AND key_sha256 NOT GLOB '*[^0-9a-f]*' AND key_bytes > 65536)),
  CHECK ((value_form = 0 AND value IS NOT NULL AND length(CAST(value AS BLOB)) <= 65536 AND value_sha256 IS NULL AND value_bytes IS NULL)
      OR (value_form = 1 AND value IS NOT NULL AND value_sha256 IS NOT NULL AND value_bytes IS NOT NULL
          AND value = '' AND length(value_sha256) = 64 AND length(CAST(value_sha256 AS BLOB)) = 64 AND value_sha256 NOT GLOB '*[^0-9a-f]*' AND value_bytes > 65536))
) STRICT;

CREATE TABLE templates (
  project_id TEXT NOT NULL,
  name TEXT NOT NULL,
  name_form INTEGER NOT NULL DEFAULT 0 CHECK (name_form IN (0,1)),
  name_sha256 TEXT, name_bytes INTEGER,
  body TEXT NOT NULL,
  body_form INTEGER NOT NULL DEFAULT 0 CHECK (body_form IN (0,1)),
  body_sha256 TEXT, body_bytes INTEGER,
  position INTEGER NOT NULL CHECK (position >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_id,name_form,name),
  CHECK ((name_form = 0 AND name IS NOT NULL AND length(CAST(name AS BLOB)) <= 65536 AND name_sha256 IS NULL AND name_bytes IS NULL)
      OR (name_form = 1 AND name IS NOT NULL AND name_sha256 IS NOT NULL AND name_bytes IS NOT NULL
          AND name = name_sha256 AND length(name_sha256) = 64 AND length(CAST(name_sha256 AS BLOB)) = 64 AND name_sha256 NOT GLOB '*[^0-9a-f]*' AND name_bytes > 65536)),
  CHECK ((body_form = 0 AND body IS NOT NULL AND length(CAST(body AS BLOB)) <= 65536 AND body_sha256 IS NULL AND body_bytes IS NULL)
      OR (body_form = 1 AND body IS NOT NULL AND body_sha256 IS NOT NULL AND body_bytes IS NOT NULL
          AND body = '' AND length(body_sha256) = 64 AND length(CAST(body_sha256 AS BLOB)) = 64 AND body_sha256 NOT GLOB '*[^0-9a-f]*' AND body_bytes > 65536))
) STRICT;

CREATE TABLE goals (
  project_id TEXT NOT NULL,
  id TEXT NOT NULL,
  id_form INTEGER NOT NULL DEFAULT 0 CHECK (id_form IN (0,1)),
  id_sha256 TEXT, id_bytes INTEGER,
  title TEXT NOT NULL,
  title_form INTEGER NOT NULL DEFAULT 0 CHECK (title_form IN (0,1)),
  title_sha256 TEXT, title_bytes INTEGER,
  description TEXT NOT NULL,
  description_form INTEGER NOT NULL DEFAULT 0 CHECK (description_form IN (0,1)),
  description_sha256 TEXT, description_bytes INTEGER,
  assignee_agent_id TEXT,
  assignee_agent_id_form INTEGER NOT NULL DEFAULT 0 CHECK (assignee_agent_id_form IN (0,1)),
  assignee_agent_id_sha256 TEXT, assignee_agent_id_bytes INTEGER,
  created_at_text TEXT NOT NULL DEFAULT '',
  created_at_text_form INTEGER NOT NULL DEFAULT 0 CHECK (created_at_text_form IN (0,1)),
  created_at_text_sha256 TEXT, created_at_text_bytes INTEGER,
  updated_at_text TEXT,
  updated_at_text_form INTEGER NOT NULL DEFAULT 0 CHECK (updated_at_text_form IN (0,1)),
  updated_at_text_sha256 TEXT, updated_at_text_bytes INTEGER,
  status TEXT NOT NULL CHECK (status IN ('active','paused','achieved','abandoned')),
  position INTEGER NOT NULL CHECK (position >= 0),
  integration_branch TEXT,
  integration_head TEXT,
  published_at INTEGER, published_commit TEXT, published_by TEXT,
  created_at INTEGER, updated_at INTEGER,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  PRIMARY KEY (project_id,id_form,id),
  CHECK ((id_form = 0 AND id IS NOT NULL AND length(CAST(id AS BLOB)) <= 65536 AND id_sha256 IS NULL AND id_bytes IS NULL)
      OR (id_form = 1 AND id IS NOT NULL AND id_sha256 IS NOT NULL AND id_bytes IS NOT NULL
          AND id = id_sha256 AND length(id_sha256) = 64 AND length(CAST(id_sha256 AS BLOB)) = 64 AND id_sha256 NOT GLOB '*[^0-9a-f]*' AND id_bytes > 65536)),
  CHECK ((title_form = 0 AND title IS NOT NULL AND length(CAST(title AS BLOB)) <= 65536 AND title_sha256 IS NULL AND title_bytes IS NULL)
      OR (title_form = 1 AND title IS NOT NULL AND title_sha256 IS NOT NULL AND title_bytes IS NOT NULL
          AND title = '' AND length(title_sha256) = 64 AND length(CAST(title_sha256 AS BLOB)) = 64 AND title_sha256 NOT GLOB '*[^0-9a-f]*' AND title_bytes > 65536)),
  CHECK ((description_form = 0 AND description IS NOT NULL AND length(CAST(description AS BLOB)) <= 65536 AND description_sha256 IS NULL AND description_bytes IS NULL)
      OR (description_form = 1 AND description IS NOT NULL AND description_sha256 IS NOT NULL AND description_bytes IS NOT NULL
          AND description = '' AND length(description_sha256) = 64 AND length(CAST(description_sha256 AS BLOB)) = 64 AND description_sha256 NOT GLOB '*[^0-9a-f]*' AND description_bytes > 65536)),
  CHECK ((assignee_agent_id_form = 0 AND (assignee_agent_id IS NULL OR (length(CAST(assignee_agent_id AS BLOB)) <= 65536)) AND assignee_agent_id_sha256 IS NULL AND assignee_agent_id_bytes IS NULL)
      OR (assignee_agent_id_form = 1 AND assignee_agent_id IS NOT NULL AND assignee_agent_id_sha256 IS NOT NULL AND assignee_agent_id_bytes IS NOT NULL
          AND assignee_agent_id = '' AND length(assignee_agent_id_sha256) = 64 AND length(CAST(assignee_agent_id_sha256 AS BLOB)) = 64 AND assignee_agent_id_sha256 NOT GLOB '*[^0-9a-f]*' AND assignee_agent_id_bytes > 65536)),
  CHECK ((created_at_text_form = 0 AND created_at_text IS NOT NULL AND length(CAST(created_at_text AS BLOB)) <= 65536 AND created_at_text_sha256 IS NULL AND created_at_text_bytes IS NULL)
      OR (created_at_text_form = 1 AND created_at_text IS NOT NULL AND created_at_text_sha256 IS NOT NULL AND created_at_text_bytes IS NOT NULL
          AND created_at_text = '' AND length(created_at_text_sha256) = 64 AND length(CAST(created_at_text_sha256 AS BLOB)) = 64 AND created_at_text_sha256 NOT GLOB '*[^0-9a-f]*' AND created_at_text_bytes > 65536)),
  CHECK ((updated_at_text_form = 0 AND (updated_at_text IS NULL OR (length(CAST(updated_at_text AS BLOB)) <= 65536)) AND updated_at_text_sha256 IS NULL AND updated_at_text_bytes IS NULL)
      OR (updated_at_text_form = 1 AND updated_at_text IS NOT NULL AND updated_at_text_sha256 IS NOT NULL AND updated_at_text_bytes IS NOT NULL
          AND updated_at_text = '' AND length(updated_at_text_sha256) = 64 AND length(CAST(updated_at_text_sha256 AS BLOB)) = 64 AND updated_at_text_sha256 NOT GLOB '*[^0-9a-f]*' AND updated_at_text_bytes > 65536))
) STRICT;
