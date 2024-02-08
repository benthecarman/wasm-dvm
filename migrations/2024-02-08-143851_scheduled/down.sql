DROP TRIGGER IF EXISTS jobs_scheduled_at_check ON jobs;
DROP FUNCTION IF EXISTS jobs_scheduled_at_check();

ALTER TABLE jobs DROP COLUMN IF EXISTS scheduled_at;
