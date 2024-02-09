// @generated automatically by Diesel CLI.

diesel::table! {
    event_jobs (job_id) {
        job_id -> Int4,
        event_id -> Int4,
    }
}

diesel::table! {
    event_nonces (id) {
        id -> Int4,
        event_id -> Int4,
        index -> Int4,
        nonce -> Bytea,
        signature -> Nullable<Bytea>,
        outcome -> Nullable<Text>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    events (id) {
        id -> Int4,
        announcement_signature -> Bytea,
        oracle_event -> Bytea,
        name -> Text,
        is_enum -> Bool,
        announcement_event_id -> Nullable<Bytea>,
        attestation_event_id -> Nullable<Bytea>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    jobs (id) {
        id -> Int4,
        payment_hash -> Bytea,
        request -> Jsonb,
        response_id -> Nullable<Bytea>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        scheduled_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    oracle_metadata (pubkey) {
        pubkey -> Bytea,
        name -> Text,
        created_at -> Timestamp,
        updated_at -> Timestamp,
        singleton_constant -> Bool,
    }
}

diesel::table! {
    zap_balances (npub) {
        npub -> Bytea,
        balance_msats -> Int4,
        created_at -> Timestamp,
    }
}

diesel::table! {
    zaps (payment_hash) {
        payment_hash -> Bytea,
        invoice -> Text,
        amount_msats -> Int4,
        request -> Jsonb,
        npub -> Bytea,
        note_id -> Nullable<Bytea>,
        created_at -> Timestamp,
    }
}

diesel::joinable!(event_jobs -> events (event_id));
diesel::joinable!(event_jobs -> jobs (job_id));
diesel::joinable!(event_nonces -> events (event_id));
diesel::joinable!(zaps -> zap_balances (npub));

diesel::allow_tables_to_appear_in_same_query!(
    event_jobs,
    event_nonces,
    events,
    jobs,
    oracle_metadata,
    zap_balances,
    zaps,
);
