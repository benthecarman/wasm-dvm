use crate::config::{Config, ServerKeys};
use crate::listener::listener_loop;
use clap::Parser;
use log::{error, info};
use nostr::{EventBuilder, Kind, Metadata, Tag, TagKind, ToBech32};
use nostr_sdk::Client;
use std::path::PathBuf;

mod config;
mod listener;
mod wasm_handler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pretty_env_logger::try_init()?;
    let config: Config = Config::parse();

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
            name: Some("Wasm DVM".to_string()),
            display_name: Some("wasm_dvm".to_string()),
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
            Tag::Generic(TagKind::Custom("k".to_string()), vec!["5988".to_string()]),
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

    let bech32 = keys.public_key().to_bech32()?;
    let http = reqwest::Client::new();
    loop {
        info!("Starting listen with key: {bech32}");
        if let Err(e) = listener_loop(&config, keys.clone(), http.clone()).await {
            error!("Error in loop: {e}");
        }
    }
}
