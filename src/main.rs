use crate::config::get_config;
use crate::node::{record, Node};
use crate::protocol::{connect, listen};
use anyhow::{Context, Result};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

mod block;
mod block_storage;
mod byte_reader;
mod config;
mod messages;
mod node;
mod protocol;
mod transaction;
mod util;
mod wallet;

fn main() -> Result<()> {
    let node = Node::shared(get_config()?);

    let (host_address, addresses_to_connect) = {
        let node = node.lock().expect("node lock poisoned");
        (
            node.config.host_address,
            node.config.addresses_to_connect.clone(),
        )
    };

    // Bound here, not in the thread: a bind failure must fail the process.
    let listener = TcpListener::bind(host_address)
        .with_context(|| format!("could not listen on {host_address}"))?;

    record(&node, format!("Listening on {}", listener.local_addr()?));

    if addresses_to_connect.is_empty() {
        record(
            &node,
            "No peers configured; waiting for inbound connections",
        );
    }

    let listening_node = Arc::clone(&node);
    let handle = thread::spawn(move || listen(listener, listening_node));

    for addr in addresses_to_connect {
        if let Err(e) = connect(addr, Arc::clone(&node)) {
            record(&node, format!("Could not connect to {addr}: {e:#}"));
        }
    }

    handle
        .join()
        .map_err(|_| anyhow::anyhow!("listener thread panicked"))?
}
