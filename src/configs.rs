use anyhow::Result;
use clap::Parser;
use serde::Deserialize;
use std::fs;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub host_address: String,
    pub addresses_to_connect: Vec<String>,
}

#[derive(Debug, Deserialize, Parser, Clone)]
struct Args {
    #[arg(long)]
    host_address: Option<String>,

    #[arg(long)]
    addresses_to_connect: Vec<String>,
}

impl ServerConfig {
    fn merge(&mut self, other: Self) -> &mut ServerConfig {
        if !other.host_address.is_empty() {
            self.host_address = other.host_address;
        }

        if !other.addresses_to_connect.is_empty() {
            self.addresses_to_connect = other.addresses_to_connect
        }

        self
    }

    fn merge_args(&mut self, other: Args) -> &mut ServerConfig {
        if let Some(host_address) = other.host_address {
            if !host_address.is_empty() {
                self.host_address = host_address;
            }
        }

        if !other.addresses_to_connect.is_empty() {
            self.addresses_to_connect = other.addresses_to_connect
        }

        self
    }
}

pub fn get_configs() -> Config {
    let mut config = get_default_configs();

    let file_configs = get_file_configs();
    if let Ok(file_configs) = file_configs {
        config.server.merge(file_configs.server);
    }

    let args = Args::parse();

    config.server.merge_args(args);
    config
}

fn get_file_configs() -> Result<Config> {
    let content = fs::read_to_string("config.toml")?;
    Ok(toml::from_str(&content).expect("Invalid config.toml"))
}

fn get_default_configs() -> Config {
    Config {
        server: ServerConfig {
            host_address: String::from("127.0.0.1:0"),
            addresses_to_connect: vec![],
        },
    }
}
