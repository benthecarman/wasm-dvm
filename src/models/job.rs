use crate::models::schema::jobs;
use diesel::{
    AsChangeset, ExpressionMethods, Identifiable, Insertable, OptionalExtension, PgConnection,
    QueryDsl, Queryable, RunQueryDsl,
};
use nostr::{Event, EventId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
#[diesel(primary_key(id))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Job {
    pub id: i32,
    payment_hash: Vec<u8>,
    request: Value,
    response_id: Option<Vec<u8>>,
    created_at: chrono::NaiveDateTime,
    updated_at: chrono::NaiveDateTime,
}

#[derive(Insertable, AsChangeset)]
#[diesel(table_name = jobs)]
struct NewJob {
    payment_hash: Vec<u8>,
    request: Value,
}

impl Job {
    pub fn request(&self) -> Event {
        serde_json::from_value(self.request.clone()).expect("invalid request")
    }

    pub fn response_id(&self) -> Option<EventId> {
        self.response_id
            .as_ref()
            .map(|v| EventId::from_slice(v).expect("invalid response id"))
    }

    pub fn create(
        conn: &mut PgConnection,
        payment_hash: [u8; 32],
        request: &Event,
    ) -> anyhow::Result<Self> {
        let new_job = NewJob {
            payment_hash: payment_hash.to_vec(),
            request: serde_json::to_value(request)?,
        };

        let res = diesel::insert_into(jobs::table)
            .values(new_job)
            .get_result::<Self>(conn)?;

        Ok(res)
    }

    pub fn get_by_payment_hash(
        conn: &mut PgConnection,
        payment_hash: &Vec<u8>,
    ) -> anyhow::Result<Option<Self>> {
        let res = jobs::table
            .filter(jobs::payment_hash.eq(payment_hash))
            .first::<Self>(conn)
            .optional()?;

        Ok(res)
    }

    pub fn set_response_id(
        conn: &mut PgConnection,
        id: i32,
        response_id: EventId,
    ) -> anyhow::Result<Self> {
        let job = diesel::update(jobs::table)
            .filter(jobs::id.eq(id))
            .set(jobs::response_id.eq(response_id.as_bytes()))
            .get_result::<Self>(conn)?;

        Ok(job)
    }
}
