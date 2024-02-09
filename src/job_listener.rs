use crate::config::Config;
use crate::invoice_subscriber::{handle_job_request, run_job_request};
use crate::models::event_job::EventJob;
use crate::models::job::Job;
use crate::models::zap_balance::ZapBalance;
use crate::models::PostgresStorage;
use crate::wasm_handler::JobParams;
use anyhow::anyhow;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgConnection;
use kormir::nostr_events::create_attestation_event;
use kormir::storage::Storage;
use kormir::{EventDescriptor, Oracle};
use lightning_invoice::Bolt11Invoice;
use log::{debug, error, info, warn};
use nostr::nips::nip04;
use nostr::prelude::{DataVendingMachineStatus, ThirtyTwoByteHash};
use nostr::{Event, EventBuilder, EventId, Filter, Keys, Kind, Tag, TagKind, Timestamp};
use nostr_sdk::{Client, RelayPoolNotification};
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use tokio::spawn;
use tokio::sync::Mutex;
use tonic_openssl_lnd::{lnrpc, LndLightningClient};

pub async fn listen_for_jobs(
    config: &Config,
    keys: Keys,
    lnd: LndLightningClient,
    db_pool: Pool<ConnectionManager<PgConnection>>,
    http: reqwest::Client,
    oracle: Oracle<PostgresStorage>,
) -> anyhow::Result<()> {
    let client = Client::new(&keys);
    client.add_relays(config.relay.clone()).await?;
    client.connect().await;

    let filter = Filter::new()
        .kind(Kind::JobRequest(5600))
        .since(Timestamp::now());

    client.subscribe(vec![filter]).await;

    let mut notifications = client.notifications();

    while let Ok(msg) = notifications.recv().await {
        match msg {
            RelayPoolNotification::Event { event, .. } => {
                if event.kind == Kind::JobRequest(5600) {
                    // spawn thread to handle event
                    let client = client.clone();
                    let keys = keys.clone();
                    let lnd = lnd.clone();
                    let db = db_pool.clone();
                    let http = http.clone();
                    let price = config.price;
                    let oracle = oracle.clone();
                    spawn(async move {
                        if let Err(e) =
                            handle_event(price, event, client, keys, lnd, db, &http, oracle).await
                        {
                            error!("Error handling event: {e}");
                        }
                    });
                }
            }
            RelayPoolNotification::Message { .. } => {}
            RelayPoolNotification::RelayStatus { .. } => {}
            RelayPoolNotification::Stop => {}
            RelayPoolNotification::Shutdown => {}
        }
    }

    client.disconnect().await?;

    Ok(())
}

pub async fn handle_event(
    price: f64,
    event: Event,
    client: Client,
    keys: Keys,
    mut lnd: LndLightningClient,
    db_pool: Pool<ConnectionManager<PgConnection>>,
    http: &reqwest::Client,
    oracle: Oracle<PostgresStorage>,
) -> anyhow::Result<()> {
    let (params, input) = get_job_params(&event, &keys)?;

    if params.time > 60 * 10 * 1_000 {
        let builder = EventBuilder::job_feedback(
            &event,
            DataVendingMachineStatus::Error,
            Some("Time must be less than 10 minutes".to_string()),
            0,
            None,
            None,
        );
        let event_id = client.send_event_builder(builder).await?;
        info!("Sent error response: {event_id}");
        return Ok(());
    }

    let value_msat = (params.time as f64 * price) as u64;

    let mut conn = db_pool.get()?;
    let balance = ZapBalance::get(&mut conn, &event.pubkey)?;

    match balance {
        Some(mut b) if (b.balance_msats as u64) >= value_msat => {
            info!(
                "User has enough balance, deducting {value_msat}msats from balance and running job"
            );
            // deduct balance
            let amt = value_msat as i32;
            b.update_balance(&mut conn, -amt)?;

            let relays = client
                .relays()
                .await
                .keys()
                .map(|r| r.to_string())
                .collect::<Vec<_>>();

            // handle job
            let job_result = handle_job_request(
                &mut conn,
                event.clone(),
                params,
                input,
                &keys,
                http,
                &oracle,
                relays,
            )
            .await?;

            if let Some(builder) = job_result.reply_event {
                let event_id = client.send_event_builder(builder).await?;
                info!("Sent response: {event_id}");

                Job::create_completed(&mut conn, &event, &event_id)?;
            }

            if let Some(oracle_announcement) = job_result.oracle_announcement {
                let event_id = client.send_event(oracle_announcement).await?;
                info!("Sent oracle announcement: {event_id}");
            }
        }
        _ => {
            let builder = create_job_feedback_invoice(
                &event,
                value_msat,
                params.schedule.map(|u| u.run_date),
                &mut lnd,
                &mut conn,
            )
            .await?;
            let event_id = client.send_event_builder(builder).await?;
            info!("Sent response: {event_id}");
        }
    }

    Ok(())
}

async fn create_job_feedback_invoice(
    event: &Event,
    value_msat: u64,
    scheduled_at: Option<u64>,
    lnd: &mut LndLightningClient,
    conn: &mut PgConnection,
) -> anyhow::Result<EventBuilder> {
    let request = lnrpc::Invoice {
        value_msat: value_msat as i64,
        memo: "Wasm DVM Request".to_string(),
        expiry: 86_400, // one day
        ..Default::default()
    };
    let resp = lnd.add_invoice(request).await?.into_inner();
    let bolt11 = resp.payment_request;
    let invoice = Bolt11Invoice::from_str(&bolt11)?;

    debug!("Created invoice: {bolt11}");

    Job::create(conn, invoice.payment_hash().into_32(), event, scheduled_at)?;

    let builder = EventBuilder::job_feedback(
        event,
        DataVendingMachineStatus::PaymentRequired,
        None,
        value_msat,
        Some(bolt11),
        None,
    );
    Ok(builder)
}

pub fn get_job_params(event: &Event, keys: &Keys) -> anyhow::Result<(JobParams, String)> {
    // if it is encrypted, decrypt the content to a tags array
    let tags = if event
        .tags
        .iter()
        .any(|t| t.kind().to_string() == "encrypted")
    {
        let p_tag = event
            .tags
            .iter()
            .find_map(|t| {
                if let Tag::PublicKey {
                    public_key,
                    uppercase: false,
                    ..
                } = t
                {
                    Some(*public_key)
                } else {
                    None
                }
            })
            .ok_or(anyhow!("Encrypted tag not found: {event:?}"))?;

        if p_tag != keys.public_key() {
            return Err(anyhow!("Params are not encrypted to us!"));
        }

        let cleartext = nip04::decrypt(&keys.secret_key()?, &event.pubkey, &event.content)?;
        let tags: Vec<Tag> = serde_json::from_str(&cleartext)?;

        tags
    } else {
        event.tags.clone()
    };

    let string = tags
        .into_iter()
        .find_map(|t| {
            if t.kind() == TagKind::I {
                let vec = t.as_vec();
                if vec.len() == 2 || (vec.len() == 3 && vec[2] == "text") {
                    Some(vec[1].clone())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .ok_or(anyhow!("Valid input tag not found: {event:?}"))?;

    let params: JobParams = serde_json::from_str(&string)?;

    Ok((params, string))
}

pub async fn process_schedule_jobs_round(
    client: &Client,
    keys: Keys,
    db_pool: Pool<ConnectionManager<PgConnection>>,
    http: reqwest::Client,
    oracle: Oracle<PostgresStorage>,
    active_jobs: Arc<Mutex<HashSet<i32>>>,
) -> anyhow::Result<()> {
    let mut conn = db_pool.get()?;
    let mut jobs = Job::get_ready_to_run_jobs(&mut conn)?;
    drop(conn);

    // update active jobs
    let mut active = active_jobs.lock().await;
    jobs.retain(|j| !active.contains(&j.id));
    active.extend(jobs.iter().map(|j| j.id));
    drop(active);

    if !jobs.is_empty() {
        info!("Running {} scheduled jobs", jobs.len());
    }

    for job in jobs {
        let client = client.clone();
        let keys = keys.clone();
        let db_pool = db_pool.clone();
        let http = http.clone();
        let oracle = oracle.clone();
        let active = active_jobs.clone();

        spawn(async move {
            if let Err(e) =
                run_scheduled_job(client, keys, db_pool, http, oracle, active, job).await
            {
                error!("Error running scheduled job: {e}");
            }
        });
    }

    Ok(())
}

async fn run_scheduled_job(
    client: Client,
    keys: Keys,
    db_pool: Pool<ConnectionManager<PgConnection>>,
    http: reqwest::Client,
    oracle: Oracle<PostgresStorage>,
    active_jobs: Arc<Mutex<HashSet<i32>>>,
    job: Job,
) -> anyhow::Result<()> {
    let event = job.request();
    let (params, input) = get_job_params(&event, &keys)?;

    let builder = run_job_request(event, params, input, &keys, &http).await?;
    let event = builder.to_event(&keys)?;
    let outcome = event.content.clone();
    let event_id = client.send_event(event).await?;
    info!("Sent response: {event_id}");

    let mut active = active_jobs.lock().await;
    active.remove(&job.id);

    let mut conn = db_pool.get()?;
    Job::set_response_id(&mut conn, job.id, event_id)?;
    // handle oracle stuff
    if let Some(event_job) = EventJob::get_by_job_id(&mut conn, job.id)? {
        if let Some(oracle_event) = oracle.storage.get_event(event_job.event_id as u32).await? {
            let outcomes = match oracle_event.announcement.oracle_event.event_descriptor {
                EventDescriptor::EnumEvent(enum_event) => enum_event.outcomes,
                EventDescriptor::DigitDecompositionEvent(_) => {
                    unimplemented!("Numeric events not implemented")
                }
            };
            if oracle_event.announcement_event_id.is_none() {
                warn!("Oracle event not announced, skipping attestation");
                return Ok(());
            }

            if outcomes.contains(&outcome) {
                let att = oracle
                    .sign_enum_event(event_job.event_id as u32, outcome)
                    .await?;
                let att_event = create_attestation_event(
                    &oracle.nostr_keys(),
                    &att,
                    EventId::from_str(&oracle_event.announcement_event_id.unwrap())?,
                )?;
                oracle
                    .storage
                    .add_attestation_event_id(event_job.event_id as u32, att_event.id)
                    .await?;
                let att_id = client.send_event(att_event).await?;
                info!("Sent oracle event outcome: {att_id}");
            } else {
                warn!("Outcome not valid for oracle event, got {outcome}, expected one of {outcomes:?}");
            }
        }
    }

    Ok(())
}
