use crate::job_listener::get_job_params;
use crate::models::Job;
use crate::wasm_handler::download_and_run_wasm;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgConnection;
use log::{error, info};
use nostr::{EventBuilder, Keys, Kind, Tag, TagKind};
use nostr_sdk::Client;
use tonic_openssl_lnd::lnrpc::invoice::InvoiceState;
use tonic_openssl_lnd::lnrpc::Invoice;
use tonic_openssl_lnd::{lnrpc, LndLightningClient};

pub async fn start_invoice_subscription(
    mut lnd: LndLightningClient,
    relays: Vec<String>,
    keys: Keys,
    http: reqwest::Client,
    db_pool: Pool<ConnectionManager<PgConnection>>,
) -> anyhow::Result<()> {
    println!("Starting invoice subscription");

    let sub = lnrpc::InvoiceSubscription::default();
    let mut invoice_stream = lnd
        .subscribe_invoices(sub)
        .await
        .expect("Failed to start invoice subscription")
        .into_inner();

    let client = Client::new(keys);
    client.add_relays(relays).await?;
    client.connect().await;

    while let Some(ln_invoice) = invoice_stream
        .message()
        .await
        .expect("Failed to receive invoices")
    {
        match InvoiceState::from_i32(ln_invoice.state) {
            Some(InvoiceState::Settled) => {
                let client = client.clone();
                let http = http.clone();
                let db_pool = db_pool.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_invoice(ln_invoice, http, client, db_pool).await {
                        error!("handle invoice error: {e}");
                    }
                });
            }
            None
            | Some(InvoiceState::Canceled)
            | Some(InvoiceState::Open)
            | Some(InvoiceState::Accepted) => {}
        }
    }

    client.disconnect().await?;

    Ok(())
}

pub async fn handle_invoice(
    ln_invoice: Invoice,
    http: reqwest::Client,
    client: Client,
    db_pool: Pool<ConnectionManager<PgConnection>>,
) -> anyhow::Result<()> {
    let mut conn = db_pool.get()?;
    let job = Job::get_by_payment_hash(&mut conn, ln_invoice.r_hash)?;

    if job.is_none() {
        return Ok(());
    }
    let job = job.unwrap();

    let event = job.request();
    let (params, string) = get_job_params(&event).expect("must have valid params");

    let result = download_and_run_wasm(params, event.id, http).await?;

    let tags = vec![
        Tag::public_key(event.pubkey),
        Tag::event(event.id),
        Tag::Generic(TagKind::I, vec![string]),
        Tag::Request(event),
    ];
    let builder = EventBuilder::new(Kind::JobResult(6600), result, tags);
    let event_id = client.send_event_builder(builder).await?;
    info!("Sent response: {event_id}");

    Job::set_response_id(&mut conn, job.id, event_id)?;

    Ok(())
}
