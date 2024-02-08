-- add trigger to make sure scheduled_at is set in the future, only on insert
CREATE OR REPLACE FUNCTION jobs_scheduled_at_check()
    RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.scheduled_at = NEW.scheduled_at THEN
        RETURN NEW;
    END IF;
    IF NEW.scheduled_at < now() THEN
        RAISE EXCEPTION 'scheduled_at must be in the future';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
