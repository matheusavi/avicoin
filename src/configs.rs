use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::fs;
use std::net::SocketAddr;

const CONFIG_FILE: &str = "config.toml";
const DEFAULT_HOST_ADDRESS: &str = "127.0.0.1:34352";

/// Fully resolved and validated configuration.
///
/// Addresses are `SocketAddr`, not `String` — parsing happens here, at the
/// boundary, so anything holding a `Config` knows the addresses are usable. A
/// typo fails at startup with a clear message rather than panicking later inside
/// whichever thread first tried to bind or connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub host_address: SocketAddr,
    pub addresses_to_connect: Vec<SocketAddr>,
}

/// The shape of `config.toml`. Every field is optional so a partial file is
/// legal and overrides only what it names.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default)]
    server: FileServerConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileServerConfig {
    #[serde(default)]
    host_address: Option<String>,
    #[serde(default)]
    addresses_to_connect: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Parser)]
#[command(name = "avicoin", about = "A Bitcoin-like cryptocurrency node")]
pub struct Args {
    /// Address this node listens on, e.g. 127.0.0.1:34352
    #[arg(long)]
    pub host_address: Option<String>,

    /// Peer address to connect to; repeat the flag for several peers
    #[arg(long)]
    pub addresses_to_connect: Vec<String>,
}

/// Reads `config.toml` and the process arguments, then resolves them.
pub fn get_configs() -> Result<Config> {
    resolve(read_file_config()?, Args::parse())
}

/// Resolves the three layers — built-in defaults, then `config.toml`, then CLI
/// arguments — each overriding the previous where it supplies a value, and
/// validates the result.
///
/// Both layers are parameters rather than being read inside, so the precedence
/// rules are testable without a filesystem or a process argv.
pub fn resolve(file: Option<FileConfig>, args: Args) -> Result<Config> {
    let file = file.unwrap_or_default().server;

    let host_address = args
        .host_address
        .or(file.host_address)
        .unwrap_or_else(|| DEFAULT_HOST_ADDRESS.to_string());

    let addresses_to_connect = if !args.addresses_to_connect.is_empty() {
        args.addresses_to_connect
    } else {
        file.addresses_to_connect.unwrap_or_default()
    };

    Ok(Config {
        host_address: parse_address(&host_address, "host_address")?,
        addresses_to_connect: addresses_to_connect
            .iter()
            .map(|a| parse_address(a, "addresses_to_connect"))
            .collect::<Result<Vec<_>>>()?,
    })
}

fn parse_address(value: &str, field: &str) -> Result<SocketAddr> {
    value
        .parse()
        .with_context(|| format!("{field}: {value:?} is not a valid address (expected host:port)"))
}

/// `Ok(None)` when there is no `config.toml`; `Err` when there is one and it
/// cannot be understood.
///
/// A missing file is fine — the defaults stand. A file that is present but
/// wrong is an error rather than a silent fallback, because a config that is
/// quietly ignored sends a node to the wrong peers with no signal.
fn read_file_config() -> Result<Option<FileConfig>> {
    let content = match fs::read_to_string(CONFIG_FILE) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("could not read {CONFIG_FILE}")),
    };

    toml::from_str(&content)
        .map(Some)
        .with_context(|| format!("{CONFIG_FILE} could not be understood"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(host: Option<&str>, peers: &[&str]) -> Args {
        Args {
            host_address: host.map(String::from),
            addresses_to_connect: peers.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn file(toml: &str) -> Option<FileConfig> {
        Some(toml::from_str(toml).expect("test fixture should be valid"))
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn defaults_apply_when_nothing_else_is_supplied() {
        let config = resolve(None, Args::default()).unwrap();

        assert_eq!(addr(DEFAULT_HOST_ADDRESS), config.host_address);
        assert!(config.addresses_to_connect.is_empty());
    }

    #[test]
    fn file_overrides_defaults() {
        let config = resolve(
            file("[server]\nhost_address = \"127.0.0.1:5000\"\naddresses_to_connect = [\"127.0.0.1:5001\"]"),
            Args::default(),
        )
        .unwrap();

        assert_eq!(addr("127.0.0.1:5000"), config.host_address);
        assert_eq!(vec![addr("127.0.0.1:5001")], config.addresses_to_connect);
    }

    #[test]
    fn a_partial_file_overrides_only_what_it_names() {
        let config = resolve(file("[server]\nhost_address = \"127.0.0.1:5000\""), Args::default())
            .unwrap();

        assert_eq!(addr("127.0.0.1:5000"), config.host_address);
        assert!(
            config.addresses_to_connect.is_empty(),
            "an omitted field should fall back to the default, not fail"
        );
    }

    #[test]
    fn an_empty_file_is_legal_and_changes_nothing() {
        let config = resolve(file(""), Args::default()).unwrap();

        assert_eq!(addr(DEFAULT_HOST_ADDRESS), config.host_address);
        assert!(config.addresses_to_connect.is_empty());
    }

    #[test]
    fn arguments_override_the_file() {
        let config = resolve(
            file("[server]\nhost_address = \"127.0.0.1:5000\"\naddresses_to_connect = [\"127.0.0.1:5001\"]"),
            args(Some("127.0.0.1:9000"), &["127.0.0.1:9001"]),
        )
        .unwrap();

        assert_eq!(addr("127.0.0.1:9000"), config.host_address);
        assert_eq!(vec![addr("127.0.0.1:9001")], config.addresses_to_connect);
    }

    #[test]
    fn absent_arguments_leave_the_file_intact() {
        let config = resolve(
            file("[server]\nhost_address = \"127.0.0.1:5000\"\naddresses_to_connect = [\"127.0.0.1:5001\"]"),
            Args::default(),
        )
        .unwrap();

        assert_eq!(addr("127.0.0.1:5000"), config.host_address);
        assert_eq!(vec![addr("127.0.0.1:5001")], config.addresses_to_connect);
    }

    #[test]
    fn several_peers_are_all_kept() {
        let config = resolve(
            None,
            args(None, &["127.0.0.1:5001", "127.0.0.1:5002", "127.0.0.1:5003"]),
        )
        .unwrap();

        assert_eq!(3, config.addresses_to_connect.len());
    }

    #[test]
    fn an_unparseable_host_address_is_rejected_by_name() {
        let error = resolve(None, args(Some("not-an-address"), &[]))
            .expect_err("an invalid address must not reach the network layer");

        let message = format!("{error:#}");
        assert!(message.contains("host_address"), "got: {message}");
        assert!(message.contains("not-an-address"), "got: {message}");
    }

    #[test]
    fn an_unparseable_peer_address_is_rejected_by_name() {
        let error = resolve(None, args(None, &["127.0.0.1:5001", "port-is-missing"]))
            .expect_err("an invalid peer address must not reach the network layer");

        let message = format!("{error:#}");
        assert!(message.contains("addresses_to_connect"), "got: {message}");
        assert!(message.contains("port-is-missing"), "got: {message}");
    }

    #[test]
    fn an_empty_host_argument_is_rejected_rather_than_ignored() {
        resolve(None, args(Some(""), &[]))
            .expect_err("an empty --host-address is a mistake, not an absent value");
    }

    #[test]
    fn an_unknown_field_in_the_file_is_rejected() {
        let parsed: std::result::Result<FileConfig, _> =
            toml::from_str("[server]\nhsot_address = \"127.0.0.1:5000\"");

        assert!(
            parsed.is_err(),
            "a misspelled key should be reported, not silently ignored"
        );
    }
}
