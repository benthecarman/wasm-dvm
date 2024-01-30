CREATE TABLE jobs
(
    id           SERIAL PRIMARY KEY,
    payment_hash bytea     NOT NULL UNIQUE,
    request      jsonb     NOT NULL UNIQUE,
    response_id  bytea UNIQUE,
    created_at   timestamp NOT NULL DEFAULT NOW(),
    updated_at   timestamp NOT NULL DEFAULT NOW()
);

create unique index jobs_payment_hash_idx on jobs (payment_hash);

-- Function to set updated_at during UPDATE
CREATE OR REPLACE FUNCTION set_updated_at()
    RETURNS TRIGGER AS
$$
BEGIN
    NEW.updated_at := CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER tr_set_dates_after_update
    BEFORE UPDATE
    ON jobs
    FOR EACH ROW
EXECUTE FUNCTION set_updated_at();
