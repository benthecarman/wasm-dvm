CREATE TABLE zap_balances
(
    npub          bytea     NOT NULL PRIMARY KEY,
    balance_msats INTEGER   NOT NULL DEFAULT 0,
    created_at    timestamp NOT NULL DEFAULT NOW()
);

CREATE TABLE zaps
(
    payment_hash bytea PRIMARY KEY,
    invoice      TEXT UNIQUE NOT NULL,
    amount_msats INTEGER     NOT NULL,
    request      jsonb       NOT NULL,
    npub         bytea       NOT NULL REFERENCES zap_balances (npub),
    note_id      bytea UNIQUE,
    created_at   timestamp   NOT NULL DEFAULT NOW()
);
