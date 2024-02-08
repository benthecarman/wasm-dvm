ALTER TABLE jobs
    ADD COLUMN scheduled_at TIMESTAMP;

-- add trigger to make sure scheduled_at is set in the future
CREATE OR REPLACE FUNCTION jobs_scheduled_at_check()
RETURNS TRIGGER AS $$
BEGIN
    IF NEW.scheduled_at < now() THEN
        RAISE EXCEPTION 'scheduled_at must be in the future';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER jobs_scheduled_at_check
    BEFORE INSERT OR UPDATE
    ON jobs
    FOR EACH ROW
    EXECUTE FUNCTION jobs_scheduled_at_check();
