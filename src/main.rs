use crate::configs::get_configs;
use crate::protocol::{connect, listen};
use anyhow::{Context, Result};
use std::net::TcpListener;
use std::thread;

mod block;
mod block_storage;
mod byte_reader;
mod configs;
mod messages;
mod protocol;
mod transaction;
mod util;
mod wallet;

fn main() -> Result<()> {
    let config = get_configs()?;

    // Bind before spawning: a bad address must fail the process, not a detached
    // thread whose panic would leave the node "running" with nothing listening.
    let listener = TcpListener::bind(config.host_address)
        .with_context(|| format!("could not listen on {}", config.host_address))?;

    println!("Listening on {}", listener.local_addr()?);

    let handle = thread::spawn(move || listen(listener));

    for addr in config.addresses_to_connect {
        thread::spawn(move || {
            if let Err(e) = connect(addr) {
                println!("Connection to {addr} ended: {e:#}");
            }
        });
    }

    handle
        .join()
        .map_err(|_| anyhow::anyhow!("listener thread panicked"))?
}
