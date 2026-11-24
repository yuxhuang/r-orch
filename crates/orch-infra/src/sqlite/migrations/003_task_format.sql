-- Full Task authority. Native tasks and memberships remain checked projections.
CREATE TABLE task_details (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL,
  format INTEGER NOT NULL CHECK (format >= 1),
  detail TEXT,
  detail_bytes INTEGER NOT NULL CHECK (detail_bytes >= 0),
  detail_sha256 TEXT NOT NULL,
  PRIMARY KEY (project_id, task_id),
  CHECK ((detail IS NULL AND detail_bytes > 65536)
      OR (detail IS NOT NULL AND detail_bytes <= 65536 AND length(CAST(detail AS BLOB)) = detail_bytes))
) STRICT;

CREATE TABLE task_blob_refs (
  project_id TEXT NOT NULL, task_id TEXT NOT NULL,
  locator TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  sha256 TEXT NOT NULL,
  size INTEGER NOT NULL CHECK (size BETWEEN 1 AND 67108864),
  PRIMARY KEY (project_id, task_id, locator, ordinal)
) STRICT, WITHOUT ROWID;
CREATE INDEX task_blob_refs_by_hash ON task_blob_refs (project_id, sha256);
