use anyhow::anyhow;
use extism::{Manifest, Plugin, Wasm};
use log::info;
use nostr::EventId;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;
use tokio::select;

const MAX_WASM_FILE_SIZE: u64 = 25_000_000; // 25mb

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobParams {
    pub url: String,
    pub function: String,
    pub input: String,
    pub time: u64,
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
    use super::{download_and_run_wasm, JobParams};
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
