use crate::models::schema::zaps;
use bitcoin::secp256k1::ThirtyTwoByteHash;
use diesel::{
    AsChangeset, ExpressionMethods, Identifiable, Insertable, OptionalExtension, PgConnection,
    QueryDsl, Queryable, RunQueryDsl,
};
use lightning_invoice::Bolt11Invoice;
use nostr::{Event, EventId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;

#[derive(
    Queryable,
    Insertable,
    Identifiable,
    AsChangeset,
    Serialize,
    Deserialize,
    Debug,
    Clone,
    PartialEq,
    Eq,
)]
#[diesel(primary_key(payment_hash))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Zap {
    payment_hash: Vec<u8>,
    invoice: String,
    pub amount_msats: i32,
    request: Value,
    npub: Vec<u8>,
    pub note_id: Option<Vec<u8>>,
    created_at: chrono::NaiveDateTime,
}

#[derive(Insertable, AsChangeset)]
#[diesel(table_name = zaps)]
struct NewZap {
    payment_hash: Vec<u8>,
    invoice: String,
    amount_msats: i32,
    request: Value,
    npub: Vec<u8>,
}

impl Zap {
    pub fn payment_hash(&self) -> [u8; 32] {
        self.payment_hash
            .clone()
            .try_into()
            .expect("Invalid length")
    }

    pub fn invoice(&self) -> Bolt11Invoice {
        Bolt11Invoice::from_str(&self.invoice).expect("Invalid invoice")
    }

    pub fn request(&self) -> Event {
        serde_json::from_value(self.request.clone()).expect("Invalid event")
    }

    pub fn npub(&self) -> nostr::PublicKey {
        nostr::PublicKey::from_slice(&self.npub).expect("Invalid key")
    }

    pub fn note_id(&self) -> Option<EventId> {
        self.note_id
            .as_ref()
            .map(|id| EventId::from_slice(id).expect("Invalid id"))
    }

    pub fn create(
        conn: &mut PgConnection,
        invoice: &Bolt11Invoice,
        request: &Event,
        for_npub: &nostr::PublicKey,
    ) -> anyhow::Result<Self> {
        let new = NewZap {
            payment_hash: invoice.payment_hash().into_32().to_vec(),
            invoice: invoice.to_string(),
            amount_msats: invoice.amount_milli_satoshis().expect("Invalid amount") as i32,
            request: serde_json::to_value(request)?,
            npub: for_npub.to_bytes().to_vec(),
        };

        let res = diesel::insert_into(zaps::table)
            .values(new)
            .get_result::<Self>(conn)?;

        Ok(res)
    }

    pub fn find_by_payment_hash(
        conn: &mut PgConnection,
        payment_hash: &Vec<u8>,
    ) -> anyhow::Result<Option<Self>> {
        let res = zaps::table
            .filter(zaps::payment_hash.eq(payment_hash))
            .first::<Self>(conn)
            .optional()?;

        Ok(res)
    }

    pub fn update_note_id(
        conn: &mut PgConnection,
        payment_hash: Vec<u8>,
        note_id: EventId,
    ) -> anyhow::Result<Self> {
        let res = diesel::update(zaps::table)
            .filter(zaps::payment_hash.eq(payment_hash))
            .set(zaps::note_id.eq(note_id.as_bytes()))
            .get_result::<Self>(conn)?;

        Ok(res)
    }
}
