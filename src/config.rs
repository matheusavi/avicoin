use crate::params::{self, Network, MAINNET};
use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "config.toml";
const DEFAULT_HOST_ADDRESS: &str = "127.0.0.1:34352";
const DATA_DIR_NAME: &str = ".avicoin";

#[derive(Debug)]
pub struct Config {
    pub host_address: SocketAddr,
    pub addresses_to_connect: Vec<SocketAddr>,
    pub network: Network,
    pub mine: bool,
    pub data_dir: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
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
    #[serde(default)]
    network: Option<String>,
    #[serde(default)]
    mine: Option<bool>,
    #[serde(default)]
    data_dir: Option<String>,
}

#[derive(Debug, Default, Parser)]
#[command(name = "avicoin", about = "A Bitcoin-like cryptocurrency node")]
struct Args {
    /// Address this node listens on, e.g. 127.0.0.1:34352
    #[arg(long)]
    host_address: Option<String>,

    /// Peer address to connect to; repeat the flag for several peers
    #[arg(long)]
    addresses_to_connect: Vec<String>,

    /// Network to join: "main" or "test". Picks a whole parameter set, and
    /// therefore a genesis block — a node on one cannot join the other
    #[arg(long)]
    network: Option<String>,

    /// Mine on this node. Without it the node relays and validates but never
    /// builds a block
    #[arg(long)]
    mine: bool,

    /// Directory this node keeps its chain, its UTXO set and its key in.
    /// Defaults to .avicoin under the home directory. One node per directory
    #[arg(long)]
    data_dir: Option<String>,
}

pub fn get_config() -> Result<Config> {
    resolve(read_file_config(CONFIG_FILE.as_ref())?, Args::parse())
}

fn resolve(file: Option<FileConfig>, args: Args) -> Result<Config> {
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

    let network = match args.network.or(file.network) {
        Some(name) => params::by_name(&name).context("network")?,
        None => &MAINNET,
    };

    let data_dir = match args.data_dir.or(file.data_dir) {
        Some(path) if path.is_empty() => bail!("data_dir: an empty path is not a directory"),
        Some(path) => PathBuf::from(path),
        None => default_data_dir(),
    };

    Ok(Config {
        network,
        data_dir,
        mine: args.mine || file.mine.unwrap_or(false),
        host_address: parse_address(&host_address, "host_address")?,
        addresses_to_connect: addresses_to_connect
            .iter()
            .map(|a| parse_address(a, "addresses_to_connect"))
            .collect::<Result<Vec<_>>>()?,
    })
}

fn default_data_dir() -> PathBuf {
    match std::env::home_dir() {
        Some(home) => home.join(DATA_DIR_NAME),
        None => PathBuf::from(DATA_DIR_NAME),
    }
}

fn parse_address(value: &str, field: &str) -> Result<SocketAddr> {
    value
        .parse()
        .with_context(|| format!("{field}: {value:?} is not a valid address (expected host:port)"))
}

fn read_file_config(path: &Path) -> Result<Option<FileConfig>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("could not read {}", path.display())),
    };

    toml::from_str(&content)
        .map(Some)
        .with_context(|| format!("{} could not be understood", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn a_node_is_on_mainnet_unless_it_is_told_otherwise() {
        let resolved = resolve(None, args(None, &[])).unwrap();

        assert_eq!(resolved.network.name, "main");
    }

    #[test]
    fn the_file_names_a_network_and_an_argument_overrides_it() {
        let from_file = resolve(file("[server]\nnetwork = \"test\""), args(None, &[])).unwrap();
        let mut overridden = args(None, &[]);
        overridden.network = Some("main".to_string());

        assert_eq!(from_file.network.name, "test");
        assert_eq!(
            resolve(file("[server]\nnetwork = \"test\""), overridden)
                .unwrap()
                .network
                .name,
            "main"
        );
    }

    #[test]
    fn a_node_does_not_mine_unless_it_is_told_to() {
        assert!(!resolve(None, args(None, &[])).unwrap().mine);
        assert!(
            resolve(file("[server]\nmine = true"), args(None, &[]))
                .unwrap()
                .mine
        );
    }

    #[test]
    fn the_flag_turns_mining_on_whatever_the_file_says() {
        let mut asked = args(None, &[]);
        asked.mine = true;

        assert!(resolve(file("[server]\nmine = false"), asked).unwrap().mine);
    }

    #[test]
    fn a_network_nobody_has_heard_of_fails_at_startup() {
        let mut invented = args(None, &[]);
        invented.network = Some("regtest".to_string());

        let error = format!("{:#}", resolve(None, invented).unwrap_err());

        assert!(error.contains("network"), "{error}");
        assert!(error.contains("regtest"), "{error}");
    }

    fn args(host: Option<&str>, peers: &[&str]) -> Args {
        Args {
            network: None,
            mine: false,
            data_dir: None,
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

    fn full_file() -> Option<FileConfig> {
        file("[server]\nhost_address = \"127.0.0.1:5000\"\naddresses_to_connect = [\"127.0.0.1:5001\"]")
    }

    #[test]
    fn defaults_apply_when_nothing_else_is_supplied() {
        let config = resolve(None, Args::default()).unwrap();

        assert_eq!(addr(DEFAULT_HOST_ADDRESS), config.host_address);
        assert!(config.addresses_to_connect.is_empty());
    }

    #[test]
    fn a_file_overrides_the_defaults_and_absent_arguments_leave_it_intact() {
        let config = resolve(full_file(), Args::default()).unwrap();

        assert_eq!(addr("127.0.0.1:5000"), config.host_address);
        assert_eq!(vec![addr("127.0.0.1:5001")], config.addresses_to_connect);
    }

    #[test]
    fn a_partial_file_overrides_only_what_it_names() {
        let config = resolve(
            file("[server]\nhost_address = \"127.0.0.1:5000\""),
            Args::default(),
        )
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
            full_file(),
            args(Some("127.0.0.1:9000"), &["127.0.0.1:9001"]),
        )
        .unwrap();

        assert_eq!(addr("127.0.0.1:9000"), config.host_address);
        assert_eq!(vec![addr("127.0.0.1:9001")], config.addresses_to_connect);
    }

    #[test]
    fn several_peers_are_all_kept() {
        let config = resolve(
            None,
            args(
                None,
                &["127.0.0.1:5001", "127.0.0.1:5002", "127.0.0.1:5003"],
            ),
        )
        .unwrap();

        assert_eq!(3, config.addresses_to_connect.len());
    }

    #[rstest]
    #[case::host(args(Some("not-an-address"), &[]), "host_address", "not-an-address")]
    #[case::peer(
        args(None, &["127.0.0.1:5001", "port-is-missing"]),
        "addresses_to_connect",
        "port-is-missing"
    )]
    fn an_unparseable_address_is_rejected_naming_the_field_and_value(
        #[case] args: Args,
        #[case] expected_field: &str,
        #[case] expected_value: &str,
    ) {
        let error =
            resolve(None, args).expect_err("an invalid address must not reach the network layer");

        let message = format!("{error:#}");
        assert!(message.contains(expected_field), "got: {message}");
        assert!(message.contains(expected_value), "got: {message}");
    }

    #[test]
    fn a_missing_config_file_is_not_an_error() {
        let absent = std::env::temp_dir().join("avicoin-no-such-config-file.toml");

        assert!(
            read_file_config(&absent).unwrap().is_none(),
            "running without a config.toml must be legal — standalone startup depends on it"
        );
    }

    #[test]
    fn a_present_but_unparseable_config_file_is_an_error() {
        let path = std::env::temp_dir().join("avicoin-unparseable-config.toml");
        fs::write(&path, "this is not toml {{{").unwrap();

        let error = read_file_config(&path)
            .expect_err("a file that is present but cannot be parsed must not be skipped silently");

        let message = format!("{error:#}");
        assert!(
            message.contains("could not be understood"),
            "the error should name the file, got: {message}"
        );

        fs::remove_file(&path).ok();
    }

    #[test]
    fn an_unreadable_config_file_is_an_error() {
        let a_directory = std::env::temp_dir();

        read_file_config(&a_directory).expect_err(
            "a config path that exists but cannot be read must not be silently skipped",
        );
    }

    #[test]
    fn an_empty_host_argument_is_rejected_rather_than_ignored() {
        resolve(None, args(Some(""), &[]))
            .expect_err("an empty --host-address is a mistake, not an absent value");
    }

    #[test]
    fn a_data_directory_defaults_and_the_file_and_an_argument_each_override_it() {
        let mut asked = args(None, &[]);
        asked.data_dir = Some("/from/the/argument".to_string());

        assert_eq!(
            resolve(None, args(None, &[]))
                .unwrap()
                .data_dir
                .file_name()
                .unwrap(),
            DATA_DIR_NAME
        );
        assert_eq!(
            resolve(
                file("[server]\ndata_dir = \"/from/the/file\""),
                args(None, &[])
            )
            .unwrap()
            .data_dir,
            PathBuf::from("/from/the/file")
        );
        assert_eq!(
            resolve(file("[server]\ndata_dir = \"/from/the/file\""), asked)
                .unwrap()
                .data_dir,
            PathBuf::from("/from/the/argument")
        );
    }

    #[test]
    fn an_empty_data_directory_is_rejected_rather_than_ignored() {
        let mut empty = args(None, &[]);
        empty.data_dir = Some(String::new());

        let error = format!("{:#}", resolve(None, empty).unwrap_err());

        assert!(error.contains("data_dir"), "{error}");
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
