-- Preserve version 1 rows; typed facts cannot be reconstructed from prose.
ALTER TABLE attention ADD COLUMN facts_json TEXT
  CHECK (facts_json IS NULL OR length(CAST(facts_json AS BLOB)) <= 512);
CREATE INDEX attention_numeric_id ON attention(project_id, CAST(id AS INTEGER));
INSERT INTO counters(project_id, name, value)
  SELECT project_id, 'attention:id', MAX(CAST(id AS INTEGER)) FROM attention GROUP BY project_id;
CREATE TRIGGER attention_checked_insert BEFORE INSERT ON attention BEGIN
  SELECT CASE WHEN NEW.id != CAST(CAST(NEW.id AS INTEGER) AS TEXT)
    OR CAST(NEW.id AS INTEGER) <= 0 OR NEW.facts_json IS NULL
    THEN RAISE(ABORT, 'attention requires canonical id and typed facts') END;
END;
CREATE TRIGGER attention_issued AFTER INSERT ON attention BEGIN
  INSERT INTO counters(project_id,name,value)
    VALUES(NEW.project_id,'attention:id',CAST(NEW.id AS INTEGER))
    ON CONFLICT(project_id,name) DO UPDATE SET value=MAX(value,excluded.value);
END;
