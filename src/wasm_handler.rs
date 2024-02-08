use anyhow::anyhow;
use extism::{Manifest, Plugin, Wasm};
use log::{debug, info};
use nostr::EventId;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io;
use std::io::{Cursor, Seek};
use std::path::PathBuf;
use std::time::Duration;
use tokio::select;
use tokio::time::Instant;

const MAX_WASM_FILE_SIZE: u64 = 25_000_000; // 25mb

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledParams {
    /// Expected outputs, if provided, a DLC announcement will be made if the output matches
    pub expected_outputs: Option<Vec<String>>,
    /// The time to run the wasm in seconds from epoch
    pub run_date: u64,
    /// What to name the announcement
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobParams {
    pub url: String,
    pub function: String,
    pub input: String,
    pub time: u64,
    pub checksum: String,
    pub schedule: Option<ScheduledParams>,
}

pub async fn download_and_run_wasm(
    job_params: JobParams,
    event_id: EventId,
    http: &reqwest::Client,
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

        let mut hasher = Sha256::new();
        io::copy(&mut content, &mut hasher)?;
        let result = hasher.finalize();
        let hex_result = format!("{result:x}");
        if job_params.checksum.to_lowercase() != hex_result {
            std::fs::remove_file(&file_path)?;
            anyhow::bail!(
                "Checksum mismatch expected: {} got: {hex_result}",
                job_params.checksum
            );
        }

        // write the file to disk
        content.rewind()?;
        io::copy(&mut content, &mut dest)?;
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
    let start = Instant::now();
    let fut = tokio::task::spawn_blocking(move || {
        plugin
            .call::<&str, &str>(&job_params.function, &job_params.input)
            .map(|x| x.to_string())
    });

    let sleep = tokio::time::sleep(Duration::from_millis(job_params.time));

    select! {
        result = fut => {
            let result = result?;
            debug!("Complete, time elapsed: {}ms", start.elapsed().as_millis());
            result
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
            time: 500,
            checksum: "93898457953d30d016f712ccf4336ce7e9971db5f7f3aff1edd252764f75d5d7"
                .to_string(),
            schedule: None,
        };
        let result = download_and_run_wasm(params, EventId::all_zeros(), &reqwest::Client::new())
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
            time: 5_000,
            checksum: "fe7ff8aaf45d67dd0d6b9fdfe3aa871e658a83adcf19c8f016013c29e8857f03"
                .to_string(),
            schedule: None,
        };
        let result = download_and_run_wasm(params, EventId::all_zeros(), &reqwest::Client::new())
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
            time: 1_000,
            checksum: "6e6386b9194f2298b5e55e88c25fe66dda454f0e2604da6964735ab1c554b513"
                .to_string(),
            schedule: None,
        };
        let err =
            download_and_run_wasm(params, EventId::all_zeros(), &reqwest::Client::new()).await;

        assert!(err.is_err());
        assert_eq!(err.unwrap_err().to_string(), "Timeout");
    }
}
