use crate::job_listener::get_job_params;
use crate::models::job::Job;
use crate::models::mark_zap_paid;
use crate::models::zap::Zap;
use crate::wasm_handler::download_and_run_wasm;
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
use nostr::{Event, EventBuilder, Keys, Kind, Tag, TagKind, ToBech32};
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
    info!("Starting invoice subscription");

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
    let job = Job::get_by_payment_hash(&mut conn, &ln_invoice.r_hash)?;

    if job.is_none() {
        // if it is not a job, try to handle it as a zap
        return handle_paid_zap(&mut conn, ln_invoice.r_hash, client).await;
    }
    let job = job.unwrap();

    let event = job.request();
    let builder = run_job_request(event, &http).await?;

    let event_id = client.send_event_builder(builder).await?;
    info!("Sent response: {event_id}");

    Job::set_response_id(&mut conn, job.id, event_id)?;

    Ok(())
}

pub async fn run_job_request(event: Event, http: &reqwest::Client) -> anyhow::Result<EventBuilder> {
    let (params, string) = get_job_params(&event).expect("must have valid params");

    let result = download_and_run_wasm(params, event.id, http).await?;

    let tags = vec![
        Tag::public_key(event.pubkey),
        Tag::event(event.id),
        Tag::Generic(TagKind::I, vec![string]),
        Tag::Request(event),
    ];
    Ok(EventBuilder::new(Kind::JobResult(6600), result, tags))
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

            let preimage = &mut [0u8; 32];
            OsRng.fill_bytes(preimage);
            let invoice_hash = sha256::Hash::hash(preimage);

            let payment_secret = &mut [0u8; 32];
            OsRng.fill_bytes(payment_secret);

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
                .payment_secret(PaymentSecret(*payment_secret))
                .min_final_cltv_expiry_delta(144)
                .basic_mpp()
                .build_signed(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, &private_key))?;

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
