-- Entity v1 Message. The registry checks empty predecessor messages before
-- any copy or schema change, then copies sibling authority in bounded batches.
CREATE TABLE entity_details_message (
  project_id TEXT NOT NULL,
  family TEXT NOT NULL CHECK (family IN ('agent','team','run','context','config','template','goal','message')),
  key_form INTEGER NOT NULL CHECK (key_form IN (0,1)),
  entity_key TEXT NOT NULL,
  format INTEGER NOT NULL CHECK (format >= 1),
  detail TEXT, detail_bytes INTEGER NOT NULL CHECK (detail_bytes >= 0),
  detail_sha256 TEXT NOT NULL CHECK (length(detail_sha256) = 64 AND length(CAST(detail_sha256 AS BLOB)) = 64 AND detail_sha256 NOT GLOB '*[^0-9a-f]*'),
  PRIMARY KEY (project_id,family,key_form,entity_key),
  CHECK ((detail IS NULL AND detail_bytes > 65536)
      OR (detail IS NOT NULL AND detail_bytes <= 65536 AND length(CAST(detail AS BLOB)) = detail_bytes)),
  CHECK ((key_form = 0 AND length(CAST(entity_key AS BLOB)) <= 65536)
      OR (key_form = 1 AND length(entity_key) = 64 AND length(CAST(entity_key AS BLOB)) = 64 AND entity_key NOT GLOB '*[^0-9a-f]*'))
) STRICT;
CREATE TABLE entity_blob_refs_message (
  project_id TEXT NOT NULL,
  family TEXT NOT NULL CHECK (family IN ('agent','team','run','context','config','template','goal','message')),
  key_form INTEGER NOT NULL CHECK (key_form IN (0,1)),
  entity_key TEXT NOT NULL, locator TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND length(CAST(sha256 AS BLOB)) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
  size INTEGER NOT NULL CHECK (size BETWEEN 0 AND 67108864 AND (size > 0 OR (family = 'run' AND locator = '/prompt'))),
  PRIMARY KEY (project_id,family,key_form,entity_key,locator,ordinal),
  CHECK ((key_form = 0 AND length(CAST(entity_key AS BLOB)) <= 65536)
      OR (key_form = 1 AND length(entity_key) = 64 AND length(CAST(entity_key AS BLOB)) = 64 AND entity_key NOT GLOB '*[^0-9a-f]*'))
) STRICT, WITHOUT ROWID;



-- Copy sibling rows here, inside the registry hook, before any DROP.
DROP TABLE entity_blob_refs;
DROP TABLE entity_details;
ALTER TABLE entity_details_message RENAME TO entity_details;
ALTER TABLE entity_blob_refs_message RENAME TO entity_blob_refs;
CREATE INDEX entity_blob_refs_by_hash ON entity_blob_refs (project_id,sha256);

DROP TABLE messages;
CREATE TABLE messages (
  project_id TEXT NOT NULL,
  id TEXT NOT NULL,
  id_form INTEGER NOT NULL DEFAULT 0 CHECK (id_form IN (0,1)),
  id_sha256 TEXT, id_bytes INTEGER,
  from_agent_id TEXT NOT NULL,
  from_agent_id_form INTEGER NOT NULL DEFAULT 0 CHECK (from_agent_id_form IN (0,1)),
  from_agent_id_sha256 TEXT, from_agent_id_bytes INTEGER,
  to_agent_id TEXT,
  to_agent_id_form INTEGER NOT NULL DEFAULT 0 CHECK (to_agent_id_form IN (0,1)),
  to_agent_id_sha256 TEXT, to_agent_id_bytes INTEGER,
  team_id TEXT,
  team_id_form INTEGER NOT NULL DEFAULT 0 CHECK (team_id_form IN (0,1)),
  team_id_sha256 TEXT, team_id_bytes INTEGER,
  reply_to TEXT,
  reply_to_form INTEGER NOT NULL DEFAULT 0 CHECK (reply_to_form IN (0,1)),
  reply_to_sha256 TEXT, reply_to_bytes INTEGER,
  subject TEXT NOT NULL,
  subject_form INTEGER NOT NULL DEFAULT 0 CHECK (subject_form IN (0,1)),
  subject_sha256 TEXT, subject_bytes INTEGER,
  body TEXT NOT NULL,
  body_form INTEGER NOT NULL DEFAULT 0 CHECK (body_form IN (0,1)),
  body_sha256 TEXT, body_bytes INTEGER,
  created_at_text TEXT NOT NULL,
  created_at_text_form INTEGER NOT NULL DEFAULT 0 CHECK (created_at_text_form IN (0,1)),
  created_at_text_sha256 TEXT, created_at_text_bytes INTEGER,
  expires_at_text TEXT,
  expires_at_text_form INTEGER NOT NULL DEFAULT 0 CHECK (expires_at_text_form IN (0,1)),
  expires_at_text_sha256 TEXT, expires_at_text_bytes INTEGER,
  delivered_at_text TEXT,
  delivered_at_text_form INTEGER NOT NULL DEFAULT 0 CHECK (delivered_at_text_form IN (0,1)),
  delivered_at_text_sha256 TEXT, delivered_at_text_bytes INTEGER,
  channel TEXT NOT NULL CHECK (channel IN ('direct','broadcast','lead')),
  status TEXT NOT NULL CHECK (status IN ('pending','delivered','expired')),
  position INTEGER NOT NULL CHECK (position >= 0),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  created_at INTEGER, expires_at INTEGER, delivered_at INTEGER,
  PRIMARY KEY (project_id,id_form,id),
  CHECK ((id_form = 0 AND id IS NOT NULL AND length(CAST(id AS BLOB)) <= 65536 AND id_sha256 IS NULL AND id_bytes IS NULL)
      OR (id_form = 1 AND id IS NOT NULL AND id_sha256 IS NOT NULL AND id_bytes IS NOT NULL
          AND id = id_sha256 AND length(id_sha256) = 64 AND length(CAST(id_sha256 AS BLOB)) = 64 AND id_sha256 NOT GLOB '*[^0-9a-f]*' AND id_bytes > 65536)),
  CHECK ((from_agent_id_form = 0 AND from_agent_id IS NOT NULL AND length(CAST(from_agent_id AS BLOB)) <= 65536 AND from_agent_id_sha256 IS NULL AND from_agent_id_bytes IS NULL)
      OR (from_agent_id_form = 1 AND from_agent_id IS NOT NULL AND from_agent_id_sha256 IS NOT NULL AND from_agent_id_bytes IS NOT NULL
          AND from_agent_id = '' AND length(from_agent_id_sha256) = 64 AND length(CAST(from_agent_id_sha256 AS BLOB)) = 64 AND from_agent_id_sha256 NOT GLOB '*[^0-9a-f]*' AND from_agent_id_bytes > 65536)),
  CHECK ((to_agent_id_form = 0 AND (to_agent_id IS NULL OR (to_agent_id IS NOT NULL AND length(CAST(to_agent_id AS BLOB)) <= 65536)) AND to_agent_id_sha256 IS NULL AND to_agent_id_bytes IS NULL)
      OR (to_agent_id_form = 1 AND to_agent_id IS NOT NULL AND to_agent_id_sha256 IS NOT NULL AND to_agent_id_bytes IS NOT NULL
          AND to_agent_id = '' AND length(to_agent_id_sha256) = 64 AND length(CAST(to_agent_id_sha256 AS BLOB)) = 64 AND to_agent_id_sha256 NOT GLOB '*[^0-9a-f]*' AND to_agent_id_bytes > 65536)),
  CHECK ((team_id_form = 0 AND (team_id IS NULL OR (team_id IS NOT NULL AND length(CAST(team_id AS BLOB)) <= 65536)) AND team_id_sha256 IS NULL AND team_id_bytes IS NULL)
      OR (team_id_form = 1 AND team_id IS NOT NULL AND team_id_sha256 IS NOT NULL AND team_id_bytes IS NOT NULL
          AND team_id = '' AND length(team_id_sha256) = 64 AND length(CAST(team_id_sha256 AS BLOB)) = 64 AND team_id_sha256 NOT GLOB '*[^0-9a-f]*' AND team_id_bytes > 65536)),
  CHECK ((reply_to_form = 0 AND (reply_to IS NULL OR (reply_to IS NOT NULL AND length(CAST(reply_to AS BLOB)) <= 65536)) AND reply_to_sha256 IS NULL AND reply_to_bytes IS NULL)
      OR (reply_to_form = 1 AND reply_to IS NOT NULL AND reply_to_sha256 IS NOT NULL AND reply_to_bytes IS NOT NULL
          AND reply_to = '' AND length(reply_to_sha256) = 64 AND length(CAST(reply_to_sha256 AS BLOB)) = 64 AND reply_to_sha256 NOT GLOB '*[^0-9a-f]*' AND reply_to_bytes > 65536)),
  CHECK ((subject_form = 0 AND subject IS NOT NULL AND length(CAST(subject AS BLOB)) <= 65536 AND subject_sha256 IS NULL AND subject_bytes IS NULL)
      OR (subject_form = 1 AND subject IS NOT NULL AND subject_sha256 IS NOT NULL AND subject_bytes IS NOT NULL
          AND subject = '' AND length(subject_sha256) = 64 AND length(CAST(subject_sha256 AS BLOB)) = 64 AND subject_sha256 NOT GLOB '*[^0-9a-f]*' AND subject_bytes > 65536)),
  CHECK ((body_form = 0 AND body IS NOT NULL AND length(CAST(body AS BLOB)) <= 65536 AND body_sha256 IS NULL AND body_bytes IS NULL)
      OR (body_form = 1 AND body IS NOT NULL AND body_sha256 IS NOT NULL AND body_bytes IS NOT NULL
          AND body = '' AND length(body_sha256) = 64 AND length(CAST(body_sha256 AS BLOB)) = 64 AND body_sha256 NOT GLOB '*[^0-9a-f]*' AND body_bytes > 65536)),
  CHECK ((created_at_text_form = 0 AND created_at_text IS NOT NULL AND length(CAST(created_at_text AS BLOB)) <= 65536 AND created_at_text_sha256 IS NULL AND created_at_text_bytes IS NULL)
      OR (created_at_text_form = 1 AND created_at_text IS NOT NULL AND created_at_text_sha256 IS NOT NULL AND created_at_text_bytes IS NOT NULL
          AND created_at_text = '' AND length(created_at_text_sha256) = 64 AND length(CAST(created_at_text_sha256 AS BLOB)) = 64 AND created_at_text_sha256 NOT GLOB '*[^0-9a-f]*' AND created_at_text_bytes > 65536)),
  CHECK ((expires_at_text_form = 0 AND (expires_at_text IS NULL OR (expires_at_text IS NOT NULL AND length(CAST(expires_at_text AS BLOB)) <= 65536)) AND expires_at_text_sha256 IS NULL AND expires_at_text_bytes IS NULL)
      OR (expires_at_text_form = 1 AND expires_at_text IS NOT NULL AND expires_at_text_sha256 IS NOT NULL AND expires_at_text_bytes IS NOT NULL
          AND expires_at_text = '' AND length(expires_at_text_sha256) = 64 AND length(CAST(expires_at_text_sha256 AS BLOB)) = 64 AND expires_at_text_sha256 NOT GLOB '*[^0-9a-f]*' AND expires_at_text_bytes > 65536)),
  CHECK ((delivered_at_text_form = 0 AND (delivered_at_text IS NULL OR (delivered_at_text IS NOT NULL AND length(CAST(delivered_at_text AS BLOB)) <= 65536)) AND delivered_at_text_sha256 IS NULL AND delivered_at_text_bytes IS NULL)
      OR (delivered_at_text_form = 1 AND delivered_at_text IS NOT NULL AND delivered_at_text_sha256 IS NOT NULL AND delivered_at_text_bytes IS NOT NULL
          AND delivered_at_text = '' AND length(delivered_at_text_sha256) = 64 AND length(CAST(delivered_at_text_sha256 AS BLOB)) = 64 AND delivered_at_text_sha256 NOT GLOB '*[^0-9a-f]*' AND delivered_at_text_bytes > 65536))
) STRICT;
CREATE INDEX messages_pending ON messages (project_id,to_agent_id_form,to_agent_id) WHERE status = 'pending';
