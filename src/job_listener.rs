use crate::config::Config;
use crate::invoice_subscriber::run_job_request;
use crate::models::job::Job;
use crate::models::zap_balance::ZapBalance;
use crate::wasm_handler::JobParams;
use anyhow::anyhow;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgConnection;
use lightning_invoice::Bolt11Invoice;
use log::{debug, error, info};
use nostr::prelude::{DataVendingMachineStatus, ThirtyTwoByteHash};
use nostr::{Event, EventBuilder, Filter, Keys, Kind, TagKind, Timestamp};
use nostr_sdk::{Client, RelayPoolNotification};
use std::str::FromStr;
use tonic_openssl_lnd::{lnrpc, LndLightningClient};

pub async fn listen_for_jobs(
    config: &Config,
    keys: Keys,
    lnd: LndLightningClient,
    db_pool: Pool<ConnectionManager<PgConnection>>,
    http: reqwest::Client,
) -> anyhow::Result<()> {
    let client = Client::new(keys);
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
                    let lnd = lnd.clone();
                    let db = db_pool.clone();
                    let http = http.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_event(event, client, lnd, db, &http).await {
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
    event: Event,
    client: Client,
    mut lnd: LndLightningClient,
    db_pool: Pool<ConnectionManager<PgConnection>>,
    http: &reqwest::Client,
) -> anyhow::Result<()> {
    let (params, _) = get_job_params(&event)?;

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

    // 1 sat per millisecond
    let value_msat = params.time * 1_000;

    let mut conn = db_pool.get()?;
    let balance = ZapBalance::get(&mut conn, &event.pubkey)?;

    let builder = match balance {
        Some(mut b) => {
            if (b.balance_msats as u64) < value_msat {
                create_job_feedback_invoice(&event, value_msat, &mut lnd, &mut conn).await?
            } else {
                info!(
                    "User has enough balance, deducting {value_msat}msats from balance and running job"
                );
                // deduct balance
                let amt = value_msat as i32;
                b.update_balance(&mut conn, -amt)?;

                // run job
                run_job_request(event, http).await?
            }
        }
        None => create_job_feedback_invoice(&event, value_msat, &mut lnd, &mut conn).await?,
    };

    let event_id = client.send_event_builder(builder).await?;
    info!("Sent response: {event_id}");

    Ok(())
}

async fn create_job_feedback_invoice(
    event: &Event,
    value_msat: u64,
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

    Job::create(conn, invoice.payment_hash().into_32(), event)?;

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

pub fn get_job_params(event: &Event) -> anyhow::Result<(JobParams, String)> {
    let string = event
        .tags
        .iter()
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
