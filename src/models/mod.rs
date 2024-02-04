use crate::models::zap::Zap;
use crate::models::zap_balance::ZapBalance;
use diesel::{Connection, PgConnection};
use diesel_migrations::{embed_migrations, EmbeddedMigrations};
use lightning_invoice::Bolt11Invoice;
use log::info;
use nostr::{Event, EventId, ToBech32};

pub mod job;
mod schema;
pub mod zap;
pub mod zap_balance;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

pub fn create_zap(
    conn: &mut PgConnection,
    invoice: &Bolt11Invoice,
    request: &Event,
) -> anyhow::Result<Zap> {
    conn.transaction(|conn| {
        let bal = ZapBalance::get(conn, &request.pubkey)?;
        if bal.is_none() {
            ZapBalance::create(conn, request.pubkey)?;
        }

        Zap::create(conn, invoice, request)
    })
}

pub fn mark_zap_paid(
    conn: &mut PgConnection,
    payment_hash: Vec<u8>,
    note_id: EventId,
) -> anyhow::Result<()> {
    conn.transaction(|conn| {
        let zap = Zap::update_note_id(conn, payment_hash, note_id)?;
        let npub = zap.npub();
        let bal = ZapBalance::get(conn, &npub)?;
        if let Some(mut bal) = bal {
            bal.update_balance(conn, zap.amount_msats)?;
            info!(
                "Updated balance for {}: {}msats",
                npub.to_bech32()?,
                bal.balance_msats
            );
        }

        Ok(())
    })
}
