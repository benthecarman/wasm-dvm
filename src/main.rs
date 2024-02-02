use crate::config::{Config, ServerKeys};
use crate::job_listener::listen_for_jobs;
use crate::models::MIGRATIONS;
use clap::Parser;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgConnection;
use diesel_migrations::MigrationHarness;
use log::{error, info};
use nostr::{EventBuilder, Kind, Metadata, Tag, TagKind, ToBech32};
use nostr_sdk::Client;
use std::path::PathBuf;
use tokio::spawn;
use tonic_openssl_lnd::lnrpc::{GetInfoRequest, GetInfoResponse};

mod config;
mod invoice_subscriber;
mod job_listener;
mod models;
mod wasm_handler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pretty_env_logger::try_init()?;
    let config: Config = Config::parse();

    // DB management
    let manager = ConnectionManager::<PgConnection>::new(&config.pg_url);
    let db_pool = Pool::builder()
        .max_size(16)
        .test_on_check_out(true)
        .build(manager)
        .expect("Could not build connection pool");

    // run migrations
    let mut conn = db_pool.get()?;
    conn.run_pending_migrations(MIGRATIONS)
        .expect("migrations could not run");
    drop(conn);

    // Create the datadir if it doesn't exist
    let mut path = PathBuf::from(&config.data_dir);
    std::fs::create_dir_all(path.clone())?;

    let keys_path = {
        path.push("keys.json");
        path
    };

    let mut server_keys = ServerKeys::get_keys(&keys_path);
    let keys = server_keys.keys();

    let mut events = vec![];
    if server_keys.kind0.is_none() {
        let metadata = Metadata {
            name: Some("wasm_dvm".to_string()),
            display_name: Some("Wasm DVM".to_string()),
            picture: Some("https://camo.githubusercontent.com/df088e16e0c36ae3804306bdf1ec1f27b0953dc5986bce126b59502a33d8072d/68747470733a2f2f692e696d6775722e636f6d2f6d58626c5233392e706e67".to_string()),
            nip05: None,
            lud16: Some("wasm-dvm@zaps.benthecarman.com".to_string()),
            ..Default::default()
        };
        let event = EventBuilder::metadata(&metadata).to_event(&keys)?;
        server_keys.kind0 = Some(event.clone());
        events.push(event)
    }
    if server_keys.kind31990.is_none() {
        let tags = vec![
            Tag::Generic(TagKind::Custom("k".to_string()), vec!["5600".to_string()]),
            Tag::Generic(
                TagKind::Custom("d".to_string()),
                vec!["9b38e816e53e412a934b0c8ff3135875".to_string()],
            ),
        ];
        let event = EventBuilder::new(
            Kind::Custom(31990),
            server_keys.kind0.as_ref().unwrap().content.clone(),
            tags,
        )
        .to_event(&keys)?;
        server_keys.kind31990 = Some(event.clone());
        events.push(event)
    }

    if !events.is_empty() {
        // send to relays
        let client = Client::new(&keys);
        client.add_relays(config.relay.clone()).await?;
        client.connect().await;
        client.batch_event(events, Default::default()).await?;
        client.disconnect().await?;
        // write to storage
        server_keys.write(&keys_path);
    }

    // connect to lnd
    let mut client = tonic_openssl_lnd::connect(
        config.lnd_host.clone(),
        config.lnd_port,
        config.cert_file(),
        config.macaroon_file(),
    )
    .await
    .expect("failed to connect");

    let mut ln_client = client.lightning().clone();
    let lnd_info: GetInfoResponse = ln_client
        .get_info(GetInfoRequest {})
        .await
        .expect("Failed to get lnd info")
        .into_inner();

    let lnd = client.lightning().clone();

    info!("Connected to LND: {}", lnd_info.identity_pubkey);

    let invoice_lnd = lnd.clone();
    let invoice_relays = config.relay.clone();
    let invoice_keys = keys.clone();
    let invoice_db_pool = db_pool.clone();
    let http = reqwest::Client::new();
    spawn(async move {
        loop {
            if let Err(e) = invoice_subscriber::start_invoice_subscription(
                invoice_lnd.clone(),
                invoice_relays.clone(),
                invoice_keys.clone(),
                http.clone(),
                invoice_db_pool.clone(),
            )
            .await
            {
                error!("Error in invoice loop: {e}");
            }
        }
    });

    let bech32 = keys.public_key().to_bech32()?;
    loop {
        info!("Starting listen with key: {bech32}");
        if let Err(e) = listen_for_jobs(&config, keys.clone(), lnd.clone(), db_pool.clone()).await {
            error!("Error in loop: {e}");
        }
    }
}
