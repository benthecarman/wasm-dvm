use crate::models::schema::zap_balances;
use bitcoin::key::XOnlyPublicKey;
use diesel::{
    AsChangeset, ExpressionMethods, Identifiable, Insertable, OptionalExtension, PgConnection,
    QueryDsl, Queryable, RunQueryDsl,
};
use serde::{Deserialize, Serialize};

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
#[diesel(primary_key(npub))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ZapBalance {
    npub: Vec<u8>,
    pub balance_msats: i32,
    created_at: chrono::NaiveDateTime,
}

#[derive(Insertable, AsChangeset)]
#[diesel(table_name = zap_balances)]
struct NewZapBalance {
    npub: Vec<u8>,
}

impl ZapBalance {
    pub fn npub(&self) -> XOnlyPublicKey {
        XOnlyPublicKey::from_slice(&self.npub).expect("Invalid key")
    }

    pub fn create(conn: &mut PgConnection, npub: XOnlyPublicKey) -> anyhow::Result<Self> {
        let new = NewZapBalance {
            npub: npub.serialize().to_vec(),
        };

        let res = diesel::insert_into(zap_balances::table)
            .values(new)
            .get_result::<Self>(conn)?;

        Ok(res)
    }

    pub fn get(conn: &mut PgConnection, npub: &XOnlyPublicKey) -> anyhow::Result<Option<Self>> {
        let res = zap_balances::table
            .filter(zap_balances::npub.eq(npub.serialize().to_vec()))
            .first::<Self>(conn)
            .optional()?;

        Ok(res)
    }

    pub fn update_balance(
        &self,
        conn: &mut PgConnection,
        amount_msats: i32,
    ) -> anyhow::Result<Self> {
        let new_balance = self.balance_msats.saturating_add(amount_msats);

        if new_balance < 0 {
            anyhow::bail!("Insufficient balance");
        }

        let res = diesel::update(zap_balances::table)
            .filter(zap_balances::npub.eq(&self.npub))
            .set(zap_balances::balance_msats.eq(new_balance))
            .get_result::<Self>(conn)?;

        Ok(res)
    }
}
