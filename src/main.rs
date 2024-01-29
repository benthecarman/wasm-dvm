use crate::config::{Config, ServerKeys};
use anyhow::anyhow;
use clap::Parser;
use log::{error, info};
use nostr::{Event, EventBuilder, Filter, Keys, Kind, Metadata, Tag, TagKind, Timestamp};
use nostr_sdk::{Client, RelayPoolNotification};
use reqwest::Url;
use std::fs::File;
use std::io::Cursor;
use std::path::PathBuf;
use wasmtime::{Engine, Instance, Module, Store};

mod config;

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

    loop {
        info!("Starting listener");
        if let Err(e) = listener_loop(&config, keys.clone()).await {
            error!("Error in loop: {e}");
        }
    }
}

pub async fn listener_loop(config: &Config, keys: Keys) -> anyhow::Result<()> {
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
                    tokio::spawn(async move {
                        if let Err(e) = handle_event(event, client).await {
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

pub async fn handle_event(event: Event, client: Client) -> anyhow::Result<()> {
    let input = event
        .tags
        .iter()
        .find_map(|t| {
            if t.kind() == TagKind::I {
                let vec = t.as_vec();
                if vec.len() == 2 || (vec.len() == 3 && vec[2] == "url") {
                    Some(vec[1].clone())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .ok_or(anyhow!("Valid input tag not found: {event:?}"))?;

    let url = Url::parse(&input)?;
    let temp_dir = tempfile::tempdir()?;
    let file_path = temp_dir.path().join(format!("{}.wasm", event.id));

    let response = reqwest::get(url).await?;

    if response.status().is_success() {
        let mut dest = File::create(&file_path)?;
        let mut content = Cursor::new(response.bytes().await?);
        std::io::copy(&mut content, &mut dest)?;
    } else {
        anyhow::bail!("Failed to download file: HTTP {}", response.status())
    };

    // todo require payment

    info!("Running wasm for event: {}", event.id);
    let result = run_wasm(&file_path)?;

    let tags = vec![
        Tag::public_key(event.pubkey),
        Tag::event(event.id),
        Tag::Generic(TagKind::I, vec![input]),
        Tag::Request(event),
        Tag::Amount {
            millisats: 10_000_000, // 10k sats
            bolt11: None,
        },
    ];
    let builder = EventBuilder::new(Kind::JobResult(6988), result.to_string(), tags);
    let event_id = client.send_event_builder(builder).await?;
    info!("Sent response: {event_id}");

    Ok(())
}

pub fn run_wasm(file_path: &PathBuf) -> anyhow::Result<i32> {
    // An engine stores and configures global compilation settings like
    // optimization level, enabled wasm features, etc.
    let engine = Engine::default();

    // We start off by creating a `Module` which represents a compiled form
    // of our input wasm module. In this case it'll be JIT-compiled after
    // we parse the text format.
    let module = Module::from_file(&engine, file_path)?;

    // A `Store` is what will own instances, functions, globals, etc. All wasm
    // items are stored within a `Store`, and it's what we'll always be using to
    // interact with the wasm world. Custom data can be stored in stores but for
    // now we just use `()`.
    let mut store = Store::new(&engine, ());

    // With a compiled `Module` we can then instantiate it, creating
    // an `Instance` which we can actually poke at functions on.
    let instance = Instance::new(&mut store, &module, &[])?;

    // The `Instance` gives us access to various exported functions and items,
    // which we access here to pull out our `run` exported function and
    // run it.
    let function = instance
        .get_func(&mut store, "run")
        .ok_or(anyhow::anyhow!("No run function"))?;

    // There's a few ways we can call the `run` `Func` value. The easiest
    // is to statically assert its signature with `typed` (in this case
    // asserting it takes no arguments and returns one i32) and then call it.
    let typed = function.typed::<(), i32>(&store)?;

    // And finally we can call our function! Note that the error propagation
    // with `?` is done to handle the case where the wasm function traps.
    let result = typed.call(&mut store, ())?;

    Ok(result)
}

#[cfg(test)]
mod test {
    use crate::run_wasm;
    use nostr::Timestamp;

    #[test]
    fn test_wasm_runner() {
        let wasm = r#"
(module
  (func (export "run") (result i32)
     i32.const 42
  )
)"#;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join(format!(
            "test_wasm_runner-{}.wasm",
            Timestamp::now().as_u64()
        ));
        std::fs::write(&file_path, wasm).unwrap();

        let result = run_wasm(&file_path).unwrap();

        assert_eq!(result, 42);
    }
}
