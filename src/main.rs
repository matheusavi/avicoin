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
mod api;
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
mod send;
mod store;
mod transaction;
mod util;
mod utxo;
mod validation;
mod wallet;

fn main() -> Result<()> {
    let arguments = config::arguments();

    // Before anything a node does: `send` claims no directory, binds no port
    // and starts no threads. It reads a key, signs, and asks a running node to
    // relay what it signed.
    if let Some(config::Command::Send {
        to,
        amount,
        fee,
        api_address,
    }) = &arguments.command
    {
        return send::send(
            &config::data_dir_of(&arguments)?,
            config::api_address_of(&arguments, api_address.as_ref())?,
            to,
            send::atoms_of(amount)?,
            crate::amount::Amount::from_atoms(*fee)?,
        );
    }

    let config = get_config(arguments)?;

    // Before anything binds: a genesis that does not satisfy its own proof of
    // work means the parameter set was edited without regenerating the nonce,
    // and a node that started anyway would be on a chain of its own.
    let network = config.network;
    let genesis = network.genesis()?;
    let genesis_hash = genesis.hash.expect("a sealed block has a hash");

    // Before the listener binds, so a wrong directory costs no port.
    let data_dir = DataDir::open(config.data_dir.clone(), network)?;

    let storage = Storage::open(&data_dir, network)?;
    let repaired = storage.discarded();
    let (node, caught_up) = Node::stored(config, &genesis, Wallet::stored(&data_dir)?, storage)?;

    let (host_address, addresses_to_connect, mining, api_address, height, tip) = {
        let node = node.lock().expect("node lock poisoned");
        (
            node.config.host_address,
            node.config.addresses_to_connect.clone(),
            node.config.mine,
            node.config.api_address,
            node.chain.height(),
            node.chain.tip(),
        )
    };

    record(
        &node,
        format!("Data directory {}", data_dir.path().display()),
    );

    if repaired > 0 {
        record(
            &node,
            format!("Repaired the block files, discarding {repaired} bytes a crash left"),
        );
    }

    record(
        &node,
        match caught_up {
            0 => format!("At height {height} on {tip}"),
            blocks => format!("At height {height} on {tip}, after connecting {blocks} from disk"),
        },
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

    match api_address {
        Some(address) => {
            // Bound here, not in the thread, so a taken port fails the process.
            let listener = api::bind(address)?;
            record(&node, format!("API on http://{address}"));

            let node = Arc::clone(&node);
            thread::spawn(move || {
                if let Err(why) = api::serve(listener, Arc::clone(&node)) {
                    record(&node, format!("API: {why:#}"));
                }
            });
        }
        None => record(&node, "No API address configured; not serving HTTP"),
    }

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
