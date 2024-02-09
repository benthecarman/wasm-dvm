use diesel::prelude::*;
use serde::{Deserialize, Serialize};

use super::schema::event_jobs;

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
)]
#[diesel(primary_key(job_id))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct EventJob {
    pub job_id: i32,
    pub event_id: i32,
}

impl EventJob {
    pub fn create(conn: &mut PgConnection, job_id: i32, event_id: i32) -> anyhow::Result<Self> {
        Ok(diesel::insert_into(event_jobs::table)
            .values(&EventJob { job_id, event_id })
            .get_result(conn)?)
    }

    pub fn get_by_job_id(conn: &mut PgConnection, job_id: i32) -> anyhow::Result<Option<Self>> {
        Ok(event_jobs::table
            .filter(event_jobs::job_id.eq(job_id))
            .first::<Self>(conn)
            .optional()?)
    }

    pub fn get_by_event_id(conn: &mut PgConnection, event_id: i32) -> anyhow::Result<Option<Self>> {
        Ok(event_jobs::table
            .filter(event_jobs::event_id.eq(event_id))
            .first::<Self>(conn)
            .optional()?)
    }
}
