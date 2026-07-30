use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::fs;

const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub host_address: String,
    pub addresses_to_connect: Vec<String>,
}

#[derive(Debug, Parser, Clone)]
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

/// Resolves configuration from three layers, each overriding the previous when
/// it has a value: built-in defaults, then `config.toml`, then CLI arguments.
///
/// A missing `config.toml` is fine — the defaults stand. A *malformed* one is an
/// error rather than a silent fallback, because a config that is quietly ignored
/// sends a node to the wrong peers with no signal that anything went wrong.
pub fn get_configs() -> Result<Config> {
    let mut config = get_default_configs();

    if let Some(file_configs) = get_file_configs()? {
        config.server.merge(file_configs.server);
    }

    config.server.merge_args(Args::parse());
    Ok(config)
}

/// `Ok(None)` when there is no `config.toml`; `Err` when there is one and it
/// cannot be parsed.
fn get_file_configs() -> Result<Option<Config>> {
    let content = match fs::read_to_string(CONFIG_FILE) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("could not read config.toml"),
    };

    toml::from_str(&content)
        .map(Some)
        .context("config.toml is present but is not valid TOML")
}

fn get_default_configs() -> Config {
    Config {
        server: ServerConfig {
            host_address: String::from("127.0.0.1:0"),
            addresses_to_connect: vec![],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> ServerConfig {
        get_default_configs().server
    }

    fn args(host: Option<&str>, peers: &[&str]) -> Args {
        Args {
            host_address: host.map(String::from),
            addresses_to_connect: peers.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn file_values_override_defaults() {
        let mut config = defaults();
        config.merge(ServerConfig {
            host_address: "127.0.0.1:5000".to_string(),
            addresses_to_connect: vec!["127.0.0.1:5001".to_string()],
        });

        assert_eq!("127.0.0.1:5000", config.host_address);
        assert_eq!(vec!["127.0.0.1:5001".to_string()], config.addresses_to_connect);
    }

    #[test]
    fn empty_file_values_leave_defaults_intact() {
        let mut config = defaults();
        let original_host = config.host_address.clone();

        config.merge(ServerConfig {
            host_address: String::new(),
            addresses_to_connect: vec![],
        });

        assert_eq!(original_host, config.host_address);
        assert!(config.addresses_to_connect.is_empty());
    }

    #[test]
    fn args_override_file_values() {
        let mut config = defaults();
        config.merge(ServerConfig {
            host_address: "127.0.0.1:5000".to_string(),
            addresses_to_connect: vec!["127.0.0.1:5001".to_string()],
        });

        config.merge_args(args(Some("127.0.0.1:9000"), &["127.0.0.1:9001"]));

        assert_eq!("127.0.0.1:9000", config.host_address);
        assert_eq!(vec!["127.0.0.1:9001".to_string()], config.addresses_to_connect);
    }

    #[test]
    fn absent_args_leave_earlier_layers_intact() {
        let mut config = defaults();
        config.merge(ServerConfig {
            host_address: "127.0.0.1:5000".to_string(),
            addresses_to_connect: vec!["127.0.0.1:5001".to_string()],
        });

        config.merge_args(args(None, &[]));

        assert_eq!("127.0.0.1:5000", config.host_address);
        assert_eq!(vec!["127.0.0.1:5001".to_string()], config.addresses_to_connect);
    }

    #[test]
    fn an_empty_host_argument_does_not_clear_the_host() {
        let mut config = defaults();
        config.merge(ServerConfig {
            host_address: "127.0.0.1:5000".to_string(),
            addresses_to_connect: vec![],
        });

        config.merge_args(args(Some(""), &[]));

        assert_eq!("127.0.0.1:5000", config.host_address);
    }

    #[test]
    fn malformed_toml_is_an_error_not_a_silent_fallback() {
        let parsed: std::result::Result<Config, _> = toml::from_str("this is not toml");
        assert!(parsed.is_err());
    }
}
