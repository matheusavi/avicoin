use crate::protocol::{connect, listen};
use serde::Deserialize;
use std::{fs, thread};

mod block;
mod block_storage;
mod byte_reader;
mod messages;
mod protocol;
mod transaction;
mod util;
mod wallet;

fn main() {
    let configs = get_configs();

    let handle = thread::spawn(|| {
        listen(configs.server.host_address).unwrap();
    });

    for addr in configs.server.addresses_to_connect {
        thread::spawn(|| connect(addr).unwrap());
    }

    handle.join().unwrap()
}

fn get_configs() -> Config {
    let config_file = fs::read_to_string("config.toml");

    let configs_result = match config_file {
        Ok(content) => toml::from_str(&content),
        _ => Ok(get_default_configs()),
    };

    let mut config = match configs_result {
        Ok(config) => config,
        _ => get_default_configs(),
    };

    if config.server.host_address.is_empty() {
        config.server.host_address = get_default_host_address()
    }
    config
}

fn get_default_configs() -> Config {
    Config {
        server: ServerConfig {
            host_address: get_default_host_address(),
            addresses_to_connect: vec![],
        },
    }
}

fn get_default_host_address() -> String {
    String::from("127.0.0.1:0")
}

#[derive(Debug, Deserialize)]
struct Config {
    server: ServerConfig,
}

#[derive(Debug, Deserialize)]
struct ServerConfig {
    host_address: String,
    addresses_to_connect: Vec<String>,
}
