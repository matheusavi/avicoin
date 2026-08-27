use crate::config::get_config;
use crate::node::{record, Node};
use crate::protocol::{keep_connected, listen, Retry};
use anyhow::{Context, Result};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

mod amount;
mod block;
mod block_storage;
mod byte_reader;
mod config;
mod crypto;
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

    // Port 0 asks the OS to pick, so only the bound address is one a peer could
    // dial us back on — and that is what `version` advertises.
    let host_address = listener.local_addr()?;
    node.lock().expect("node lock poisoned").config.host_address = host_address;

    record(&node, format!("Listening on {host_address}"));

    if addresses_to_connect.is_empty() {
        record(
            &node,
            "No peers configured; waiting for inbound connections",
        );
    }

    let listening_node = Arc::clone(&node);
    let handle = thread::spawn(move || listen(listener, listening_node));

    for addr in addresses_to_connect {
        let node = Arc::clone(&node);
        thread::spawn(move || keep_connected(addr, node, Retry::default()));
    }

    handle
        .join()
        .map_err(|_| anyhow::anyhow!("listener thread panicked"))?
}
