use crate::config::get_config;
use crate::data_dir::DataDir;
use crate::miner::Throttle;
use crate::node::{record, Node};
use crate::persist::Storage;
use crate::protocol::{keep_connected, listen, Retry};
use crate::util::display_order;
use crate::wallet::Wallet;
use anyhow::{Context, Result};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

mod address;
mod amount;
mod block;
mod block_storage;
mod blockchain;
mod byte_reader;
mod config;
mod crypto;
mod data_dir;
mod difficulty;
mod mempool;
mod messages;
mod miner;
mod node;
mod params;
mod persist;
mod protocol;
mod script;
mod store;
mod transaction;
mod util;
mod utxo;
mod validation;
mod wallet;

fn main() -> Result<()> {
    let config = get_config()?;

    // Before anything binds: a genesis that does not satisfy its own proof of
    // work means the parameter set was edited without regenerating the nonce,
    // and a node that started anyway would be on a chain of its own.
    let network = config.network;
    let genesis = network.genesis()?;
    let genesis_hash = genesis.hash.expect("a sealed block has a hash");

    // Before the listener binds, so a wrong directory costs no port.
    let data_dir = DataDir::open(config.data_dir.clone(), network)?;

    let storage = Storage::open(&data_dir, network)?;
    let (node, caught_up) = Node::stored(config, &genesis, Wallet::stored(&data_dir)?, storage)?;

    let (host_address, addresses_to_connect, mining, height, tip) = {
        let node = node.lock().expect("node lock poisoned");
        (
            node.config.host_address,
            node.config.addresses_to_connect.clone(),
            node.config.mine,
            node.chain.height(),
            node.chain.tip(),
        )
    };

    record(
        &node,
        format!("Data directory {}", data_dir.path().display()),
    );

    record(
        &node,
        format!(
            "On the {} network, genesis {}",
            network.name,
            hex::encode(display_order(genesis_hash))
        ),
    );

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

    if mining {
        let node = Arc::clone(&node);
        record(&node, "Mining");
        thread::spawn(move || miner::mine(node, Throttle::default()));
    }

    for addr in addresses_to_connect {
        let node = Arc::clone(&node);
        thread::spawn(move || keep_connected(addr, node, Retry::default()));
    }

    handle
        .join()
        .map_err(|_| anyhow::anyhow!("listener thread panicked"))?
}
