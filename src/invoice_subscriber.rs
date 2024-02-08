use crate::job_listener::get_job_params;
use crate::models::job::Job;
use crate::models::mark_zap_paid;
use crate::models::zap::Zap;
use crate::wasm_handler::{download_and_run_wasm, JobParams};
use bitcoin::hashes::sha256;
use bitcoin::hashes::Hash;
use bitcoin::key::Secp256k1;
use bitcoin::secp256k1::rand::rngs::OsRng;
use bitcoin::secp256k1::rand::RngCore;
use bitcoin::secp256k1::SecretKey;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgConnection;
use lightning_invoice::{Currency, InvoiceBuilder, PaymentSecret};
use log::{error, info};
use nostr::nips::nip04;
use nostr::prelude::DataVendingMachineStatus;
use nostr::{Event, EventBuilder, Keys, Kind, Tag, TagKind, ToBech32};
use nostr_sdk::Client;
use std::time::{SystemTime, UNIX_EPOCH};
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
    info!("Starting invoice subscription");

    let sub = lnrpc::InvoiceSubscription::default();
    let mut invoice_stream = lnd
        .subscribe_invoices(sub)
        .await
        .expect("Failed to start invoice subscription")
        .into_inner();

    let client = Client::new(&keys);
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
                let keys = keys.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_invoice(ln_invoice, http, client, &keys, db_pool).await {
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
    keys: &Keys,
    db_pool: Pool<ConnectionManager<PgConnection>>,
) -> anyhow::Result<()> {
    let mut conn = db_pool.get()?;
    let job = Job::get_by_payment_hash(&mut conn, &ln_invoice.r_hash)?;

    if job.is_none() {
        // if it is not a job, try to handle it as a zap
        return handle_paid_zap(&mut conn, ln_invoice.r_hash, client).await;
    }
    let job = job.unwrap();

    let event = job.request();
    let (params, input) = get_job_params(&event, keys).expect("must have valid params");
    let builder = handle_job_request(&mut conn, event, params, input, keys, &http).await?;

    if let Some(builder) = builder {
        let event_id = client.send_event_builder(builder).await?;
        info!("Sent response: {event_id}");

        Job::set_response_id(&mut conn, job.id, event_id)?;
    }

    Ok(())
}

pub async fn handle_job_request(
    conn: &mut PgConnection,
    event: Event,
    params: JobParams,
    input: String,
    keys: &Keys,
    http: &reqwest::Client,
) -> anyhow::Result<Option<EventBuilder>> {
    match params.schedule.as_ref() {
        Some(schedule) => {
            // todo make DLC oracle
            if schedule.run_date <= SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() {
                run_job_request(event, params, input, keys, http)
                    .await
                    .map(Some)
            } else {
                // schedule job for future
                Job::create_scheduled(conn, &event, schedule.run_date)?;
                Ok(None)
            }
        }
        None => run_job_request(event, params, input, keys, http)
            .await
            .map(Some),
    }
}

pub async fn run_job_request(
    event: Event,
    params: JobParams,
    input: String,
    keys: &Keys,
    http: &reqwest::Client,
) -> anyhow::Result<EventBuilder> {
    match download_and_run_wasm(params, event.id, http).await {
        Ok(result) => {
            let mut tags = vec![
                Tag::public_key(event.pubkey),
                Tag::event(event.id),
                Tag::Generic(TagKind::I, vec![input]),
                Tag::Request(event.clone()),
            ];

            if event
                .tags
                .iter()
                .any(|t| t.kind().to_string() == "encrypted")
            {
                tags.push(Tag::Generic(
                    TagKind::Custom("encrypted".to_string()),
                    vec![],
                ));
                let encrypted = nip04::encrypt(&keys.secret_key()?, &event.pubkey, result)?;
                Ok(EventBuilder::new(Kind::JobResult(6600), encrypted, tags))
            } else {
                Ok(EventBuilder::new(Kind::JobResult(6600), result, tags))
            }
        }
        Err(e) => {
            error!("Error running event {}: {e}", event.id);
            Ok(EventBuilder::job_feedback(
                &event,
                DataVendingMachineStatus::Error,
                Some(e.to_string()),
                0,
                None,
                None,
            ))
        }
    }
}

async fn handle_paid_zap(
    conn: &mut PgConnection,
    payment_hash: Vec<u8>,
    client: Client,
) -> anyhow::Result<()> {
    match Zap::find_by_payment_hash(conn, &payment_hash)? {
        None => Ok(()),
        Some(zap) => {
            if zap.note_id.is_some() {
                return Ok(());
            }

            let invoice = zap.invoice();

            let mut preimage = [0u8; 32];
            OsRng.fill_bytes(&mut preimage);
            let invoice_hash = sha256::Hash::hash(&preimage);

            let mut payment_secret = [0u8; 32];
            OsRng.fill_bytes(&mut payment_secret);

            let private_key = SecretKey::new(&mut OsRng);

            let amt_msats = invoice
                .amount_milli_satoshis()
                .expect("Invoice must have an amount");

            let zap_request = zap.request();

            info!(
                "Received zap for {amt_msats} msats from {}!",
                zap_request.pubkey.to_bech32()?
            );

            let fake_invoice = InvoiceBuilder::new(Currency::Bitcoin)
                .amount_milli_satoshis(amt_msats)
                .invoice_description(invoice.description())
                .current_timestamp()
                .payment_hash(invoice_hash)
                .payment_secret(PaymentSecret(payment_secret))
                .min_final_cltv_expiry_delta(144)
                .basic_mpp()
                .build_signed(|hash| {
                    Secp256k1::signing_only().sign_ecdsa_recoverable(hash, &private_key)
                })?;

            let event = EventBuilder::zap_receipt(
                fake_invoice.to_string(),
                Some(hex::encode(preimage)),
                zap_request,
            );

            let event_id = client.send_event_builder(event).await?;

            info!(
                "Broadcasted zap event id: {}!",
                event_id.to_bech32().expect("bech32")
            );

            mark_zap_paid(conn, payment_hash, event_id)?;

            Ok(())
        }
    }
}
