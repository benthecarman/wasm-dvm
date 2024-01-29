use crate::config::Config;
use crate::wasm_handler::{download_and_run_wasm, JobParams};
use anyhow::anyhow;
use log::{error, info};
use nostr::{Event, EventBuilder, Filter, Keys, Kind, Tag, TagKind, Timestamp};
use nostr_sdk::{Client, RelayPoolNotification};

pub async fn listener_loop(
    config: &Config,
    keys: Keys,
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
                    let http = http.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_event(event, client, http).await {
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

    Ok(())
}

pub async fn handle_event(
    event: Event,
    client: Client,
    http: reqwest::Client,
) -> anyhow::Result<()> {
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

    let result = download_and_run_wasm(params, event.id, http).await?;

    let tags = vec![
        Tag::public_key(event.pubkey),
        Tag::event(event.id),
        Tag::Generic(TagKind::I, vec![string]),
        Tag::Request(event),
        Tag::Amount {
            millisats: 10_000_000, // 10k sats
            bolt11: None,
        },
    ];
    let builder = EventBuilder::new(Kind::JobResult(6600), result, tags);
    let event_id = client.send_event_builder(builder).await?;
    info!("Sent response: {event_id}");

    Ok(())
}
