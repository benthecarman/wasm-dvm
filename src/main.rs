use std::fs::File;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use clap::Parser;
use extism::{Manifest, Plugin, Wasm};
use log::{error, info};
use nostr::{Event, EventBuilder, EventId, Filter, Keys, Kind, Metadata, Tag, TagKind, Timestamp};
use nostr_sdk::{Client, RelayPoolNotification};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::select;

use crate::config::{Config, ServerKeys};

mod config;

const MAX_WASM_FILE_SIZE: u64 = 25_000_000; // 25mb

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobParams {
    pub url: String,
    pub function: String,
    pub input: String,
    pub time: u64,
}

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
            picture: None,
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

    let http = reqwest::Client::new();
    loop {
        info!("Starting listener");
        if let Err(e) = listener_loop(&config, keys.clone(), http.clone()).await {
            error!("Error in loop: {e}");
        }
    }
}

pub async fn listener_loop(
    config: &Config,
    keys: Keys,
    http: reqwest::Client,
) -> anyhow::Result<()> {
    let client = Client::new(keys);
    client.add_relays(config.relay.clone()).await?;
    client.connect().await;

    let filter = Filter::new()
        .kind(Kind::JobRequest(5988))
        .since(Timestamp::now());

    client.subscribe(vec![filter]).await;

    let mut notifications = client.notifications();

    while let Ok(msg) = notifications.recv().await {
        match msg {
            RelayPoolNotification::Event { event, .. } => {
                if event.kind == Kind::JobRequest(5988) {
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
    let builder = EventBuilder::new(Kind::JobResult(6988), result, tags);
    let event_id = client.send_event_builder(builder).await?;
    info!("Sent response: {event_id}");

    Ok(())
}

pub async fn download_and_run_wasm(
    job_params: JobParams,
    event_id: EventId,
    http: reqwest::Client,
) -> anyhow::Result<String> {
    let url = Url::parse(&job_params.url)?;
    let temp_dir = tempfile::tempdir()?;
    let file_path = temp_dir.path().join(format!("{event_id}.wasm"));

    let response = http.get(url).send().await?;

    if response.status().is_success() {
        // if length larger than 25mb, error
        if response.content_length().unwrap_or(0) > MAX_WASM_FILE_SIZE {
            anyhow::bail!("File too large");
        }

        let mut dest = File::create(&file_path)?;

        let bytes = response.bytes().await?;
        if bytes.len() as u64 > MAX_WASM_FILE_SIZE {
            anyhow::bail!("File too large");
        }
        let mut content = Cursor::new(bytes);

        std::io::copy(&mut content, &mut dest)?;
    } else {
        anyhow::bail!("Failed to download file: HTTP {}", response.status())
    };

    info!("Running wasm for event: {event_id}");
    run_wasm(file_path, job_params).await
}

pub async fn run_wasm(file_path: PathBuf, job_params: JobParams) -> anyhow::Result<String> {
    let wasm = Wasm::file(file_path);
    let mut manifest = Manifest::new([wasm]);
    manifest.allowed_hosts = Some(vec!["*".to_string()]);
    let mut plugin = Plugin::new(manifest, [], true)?;
    let cancel_handle = plugin.cancel_handle();
    let fut = tokio::task::spawn_blocking(move || {
        plugin
            .call::<&str, &str>(&job_params.function, &job_params.input)
            .map(|x| x.to_string())
    });

    let sleep = tokio::time::sleep(Duration::from_secs(job_params.time));

    select! {
        result = fut => {
            result?
        }
        _ = sleep => {
            cancel_handle.cancel()?;
            Err(anyhow!("Timeout"))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{download_and_run_wasm, JobParams};
    use nostr::EventId;
    use serde_json::Value;

    #[tokio::test]
    async fn test_wasm_runner() {
        let params = JobParams {
            url: "https://github.com/extism/plugins/releases/download/v0.5.0/count_vowels.wasm"
                .to_string(),
            function: "count_vowels".to_string(),
            input: "Hello World".to_string(),
            time: 5,
        };
        let result = download_and_run_wasm(params, EventId::all_zeros(), reqwest::Client::new())
            .await
            .unwrap();

        assert_eq!(
            result,
            "{\"count\":3,\"total\":3,\"vowels\":\"aeiouAEIOU\"}"
        );
    }

    #[tokio::test]
    async fn test_http_wasm() {
        let params = JobParams {
            url: "https://github.com/extism/plugins/releases/download/v0.5.0/http.wasm".to_string(),
            function: "http_get".to_string(),
            input: "{\"url\":\"https://benthecarman.com/.well-known/nostr.json\"}".to_string(), // get my nip05
            time: 5,
        };
        let result = download_and_run_wasm(params, EventId::all_zeros(), reqwest::Client::new())
            .await
            .unwrap();

        let json = serde_json::from_str::<Value>(&result);

        assert!(json.is_ok());
    }

    #[tokio::test]
    async fn test_timeout_infinite_loop() {
        let params = JobParams {
            url: "https://github.com/extism/plugins/releases/download/v0.5.0/loop_forever.wasm"
                .to_string(),
            function: "loop_forever".to_string(),
            input: "".to_string(),
            time: 1,
        };
        let err = download_and_run_wasm(params, EventId::all_zeros(), reqwest::Client::new()).await;

        assert!(err.is_err());
        assert_eq!(err.unwrap_err().to_string(), "Timeout");
    }
}
