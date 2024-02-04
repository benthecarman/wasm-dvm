use clap::Parser;
use nostr::bitcoin::Network;
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
    /// Postgres connection string
    #[clap(long)]
    pub pg_url: String,
    /// Location keys files
    #[clap(default_value = ".", long)]
    pub data_dir: String,
    /// Relay to connect to, can be specified multiple times
    #[clap(short, long)]
    pub relay: Vec<String>,
    /// Host of the GRPC server for lnd
    #[clap(default_value_t = String::from("127.0.0.1"), long)]
    pub lnd_host: String,
    /// Port of the GRPC server for lnd
    #[clap(default_value_t = 10009, long)]
    pub lnd_port: u32,
    /// Network lnd is running on ["bitcoin", "testnet", "signet, "regtest"]
    #[clap(default_value_t = Network::Bitcoin, short, long)]
    pub network: Network,
    /// Path to tls.cert file for lnd
    #[clap(long)]
    cert_file: Option<String>,
    /// Path to admin.macaroon file for lnd
    #[clap(long)]
    macaroon_file: Option<String>,
    /// The domain name you are running the lnurl server on
    #[clap(default_value_t = String::from("localhost:3000"), long)]
    pub domain: String,
    /// Bind address for webserver
    #[clap(default_value_t = String::from("0.0.0.0"), long)]
    pub bind: String,
    /// Port for webserver
    #[clap(default_value_t = 3000, long)]
    pub port: u16,
    /// How many millisats per millisecond of runtime
    #[clap(default_value_t = 1.0, long)]
    pub price: f64,
}

impl Config {
    pub fn macaroon_file(&self) -> String {
        self.macaroon_file
            .clone()
            .unwrap_or_else(|| default_macaroon_file(&self.network))
    }

    pub fn cert_file(&self) -> String {
        self.cert_file.clone().unwrap_or_else(default_cert_file)
    }
}

fn home_directory() -> String {
    let buf = home::home_dir().expect("Failed to get home dir");
    let str = format!("{}", buf.display());

    // to be safe remove possible trailing '/' and
    // we can manually add it to paths
    match str.strip_suffix('/') {
        Some(stripped) => stripped.to_string(),
        None => str,
    }
}

pub fn default_cert_file() -> String {
    format!("{}/.lnd/tls.cert", home_directory())
}

pub fn default_macaroon_file(network: &Network) -> String {
    let network_str = match network {
        Network::Bitcoin => "mainnet",
        Network::Testnet => "testnet",
        Network::Signet => "signet",
        Network::Regtest => "regtest",
        _ => panic!("Unsupported network"),
    };

    format!(
        "{}/.lnd/data/chain/bitcoin/{}/admin.macaroon",
        home_directory(),
        network_str
    )
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
