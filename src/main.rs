use crate::config::get_config;
use crate::protocol::{connect, listen};
use anyhow::{Context, Result};
use std::net::TcpListener;
use std::thread;

mod block;
mod block_storage;
mod byte_reader;
mod config;
mod messages;
mod protocol;
mod transaction;
mod util;
mod wallet;

fn main() -> Result<()> {
    let config = get_config()?;

    // Bound here, not in the thread: a bind failure must fail the process.
    let listener = TcpListener::bind(config.host_address)
        .with_context(|| format!("could not listen on {}", config.host_address))?;

    println!("Listening on {}", listener.local_addr()?);

    if config.addresses_to_connect.is_empty() {
        println!("No peers configured; waiting for inbound connections");
    }

    let handle = thread::spawn(move || listen(listener));

    for addr in config.addresses_to_connect {
        if let Err(e) = connect(addr) {
            println!("Could not connect to {addr}: {e:#}");
        }
    }

    handle
        .join()
        .map_err(|_| anyhow::anyhow!("listener thread panicked"))?
}
