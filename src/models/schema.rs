// @generated automatically by Diesel CLI.

diesel::table! {
    jobs (id) {
        id -> Int4,
        payment_hash -> Bytea,
        request -> Jsonb,
        response_id -> Nullable<Bytea>,
        created_at -> Timestamp,
        updated_at -> Timestamp,
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

diesel::joinable!(zaps -> zap_balances (npub));

diesel::allow_tables_to_appear_in_same_query!(
    jobs,
    zap_balances,
    zaps,
);
