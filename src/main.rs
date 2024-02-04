use crate::config::{Config, ServerKeys};
use crate::job_listener::listen_for_jobs;
use crate::models::MIGRATIONS;
use crate::routes::{get_invoice, get_lnurl_pay, get_nip05};
use axum::http::{Method, StatusCode, Uri};
use axum::routing::get;
use axum::{http, Extension, Router};
use clap::Parser;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgConnection;
use diesel_migrations::MigrationHarness;
use log::{error, info};
use nostr::{EventBuilder, Keys, Kind, Metadata, Tag, TagKind, ToBech32};
use nostr_sdk::Client;
use std::path::PathBuf;
use tokio::signal::unix::{signal, SignalKind};
use tokio::spawn;
use tokio::sync::oneshot;
use tonic_openssl_lnd::lnrpc::{GetInfoRequest, GetInfoResponse};
use tonic_openssl_lnd::LndLightningClient;
use tower_http::cors::{Any, CorsLayer};

mod config;
mod invoice_subscriber;
mod job_listener;
mod models;
mod routes;
mod wasm_handler;

#[derive(Clone)]
pub struct State {
    pub db_pool: Pool<ConnectionManager<PgConnection>>,
    pub lnd: LndLightningClient,
    pub keys: Keys,
    pub relays: Vec<String>,
    pub domain: String,
}

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
            nip05: Some(format!("_@{}", config.domain)),
            lud16: Some(format!("wasm-dvm@{}", config.domain)),
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
                TagKind::D,
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
        info!("Broadcasted metadata events");
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

    let http = reqwest::Client::new();

    let invoice_lnd = lnd.clone();
    let invoice_relays = config.relay.clone();
    let invoice_keys = keys.clone();
    let invoice_db_pool = db_pool.clone();
    let invoice_http = http.clone();
    spawn(async move {
        loop {
            if let Err(e) = invoice_subscriber::start_invoice_subscription(
                invoice_lnd.clone(),
                invoice_relays.clone(),
                invoice_keys.clone(),
                invoice_http.clone(),
                invoice_db_pool.clone(),
            )
            .await
            {
                error!("Error in invoice loop: {e}");
            }
        }
    });

    let bech32 = keys.public_key().to_bech32()?;
    let jobs_config = config.clone();
    let jobs_keys = keys.clone();
    let jobs_lnd = lnd.clone();
    let jobs_db_pool = db_pool.clone();
    spawn(async move {
        loop {
            info!("Starting listen with key: {bech32}");
            if let Err(e) = listen_for_jobs(
                &jobs_config,
                jobs_keys.clone(),
                jobs_lnd.clone(),
                jobs_db_pool.clone(),
                http.clone(),
            )
            .await
            {
                error!("Error in loop: {e}");
            }
        }
    });

    let state = State {
        db_pool,
        lnd,
        keys,
        relays: config.relay.clone(),
        domain: config.domain.clone(),
    };

    let addr: std::net::SocketAddr = format!("{}:{}", config.bind, config.port)
        .parse()
        .expect("Failed to parse bind/port for webserver");

    info!("Webserver running on http://{}", addr);

    let server_router = Router::new()
        .route("/get-invoice/:hash", get(get_invoice))
        .route("/.well-known/lnurlp/:name", get(get_lnurl_pay))
        .route("/.well-known/nostr.json", get(get_nip05))
        .fallback(fallback)
        .layer(Extension(state))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_headers(vec![http::header::CONTENT_TYPE])
                .allow_methods([Method::GET, Method::POST]),
        );

    let server = axum::Server::bind(&addr).serve(server_router.into_make_service());

    // Set up a oneshot channel to handle shutdown signal
    let (tx, rx) = oneshot::channel();

    // Spawn a task to listen for shutdown signals
    spawn(async move {
        let mut term_signal = signal(SignalKind::terminate())
            .map_err(|e| error!("failed to install TERM signal handler: {e}"))
            .unwrap();
        let mut int_signal = signal(SignalKind::interrupt())
            .map_err(|e| {
                error!("failed to install INT signal handler: {e}");
            })
            .unwrap();

        tokio::select! {
            _ = term_signal.recv() => {
                info!("Received SIGTERM");
            },
            _ = int_signal.recv() => {
                info!("Received SIGINT");
            },
        }

        let _ = tx.send(());
    });

    let graceful = server.with_graceful_shutdown(async {
        let _ = rx.await;
    });

    // Await the server to receive the shutdown signal
    if let Err(e) = graceful.await {
        error!("shutdown error: {}", e);
    }

    info!("Graceful shutdown complete");

    Ok(())
}

async fn fallback(uri: Uri) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, format!("No route for {}", uri))
}
