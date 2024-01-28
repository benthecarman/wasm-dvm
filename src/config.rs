use clap::Parser;
use nostr::key::SecretKey;
use nostr::{Event, Keys};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(version, author, about)]
/// A tool for zapping based on reactions to notes.
pub struct Config {
    /// Location keys files
    #[clap(default_value = ".", long)]
    pub data_dir: String,
    /// Relay to connect to, can be specified multiple times
    #[clap(short, long)]
    pub relay: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerKeys {
    server_key: SecretKey,
    pub kind0: Option<Event>,
    pub kind31990: Option<Event>,
}

impl ServerKeys {
    fn generate() -> Self {
        let server_key = Keys::generate();

        ServerKeys {
            server_key: server_key.secret_key().unwrap(),
            kind0: None,
            kind31990: None,
        }
    }

    pub fn keys(&self) -> Keys {
        Keys::new(self.server_key)
    }

    pub fn get_keys(path: &PathBuf) -> ServerKeys {
        match File::open(path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                serde_json::from_reader(reader).expect("Could not parse JSON")
            }
            Err(_) => {
                let keys = ServerKeys::generate();
                keys.write(path);

                keys
            }
        }
    }

    pub fn write(&self, path: &PathBuf) {
        let json_str = serde_json::to_string(&self).expect("Could not serialize data");

        let mut file = File::create(path).expect("Could not create file");
        file.write_all(json_str.as_bytes())
            .expect("Could not write to file");
    }
}
