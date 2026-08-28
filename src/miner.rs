use crate::amount::{subsidy, Amount};
use crate::block::{Block, BlockHash, HEADER_SIZE};
use crate::blockchain::Accepted;
use crate::mempool::Entry;
use crate::messages::inventory::{Inventory, Item};
use crate::messages::message::Message;
use crate::node::{record, SharedNode};
use crate::script::p2pkh;
use crate::transaction::Transaction;
use crate::util::now;
use crate::validation::{MAX_BLOCK_SIZE, MAX_COINBASE_DATA};
use anyhow::{Context, Result};
use std::thread;
use std::time::Duration;

/// How the miner stays a guest on a machine that has other work. The public
/// node runs on very little compute and the point is a chain that advances,
/// not a saturated core — difficulty adapts to whatever hashrate results, so
/// this is a runtime knob and never a consensus concern.
#[derive(Clone, Copy, Debug)]
pub struct Throttle {
    pub burst: u32,
    pub rest: Duration,
}

impl Default for Throttle {
    fn default() -> Self {
        Throttle {
            burst: 50_000,
            rest: Duration::from_millis(50),
        }
    }
}

/// What the miner needs from the node, taken under one lock and worked on
/// without it. Every field is a copy: nothing here borrows the node.
struct Candidate {
    block: Block,
    on: BlockHash,
}

/// Never returns. A miner that gave up on the first error it met would stop
/// mining for the rest of the process with nothing to show for it, so every
/// error is recorded and the next candidate is built.
pub fn mine(node: SharedNode, throttle: Throttle) {
    loop {
        if let Err(why) = attempt(&node, throttle) {
            record(&node, format!("Mining: {why:#}"));
            thread::sleep(throttle.rest);
        }
    }
}

fn attempt(node: &SharedNode, throttle: Throttle) -> Result<()> {
    let Some(mut candidate) = build(node)? else {
        // The chain has caught up with the clock. Mining on would put a
        // timestamp in the future, and enough of those in a row is a chain
        // every other node refuses.
        thread::sleep(throttle.rest);
        return Ok(());
    };

    match grind(&mut candidate, node, throttle) {
        Some(solved) => submit(node, solved),
        None => Ok(()),
    }
}

/// Snapshots the tip and the mempool, then lets the lock go. Everything after
/// this — building, hashing — happens without it.
fn build(node: &SharedNode) -> Result<Option<Candidate>> {
    let (on, height, n_bits, parent_time, address, mut entries) = {
        let held = node.lock().expect("node lock poisoned");
        let on = held.chain.tip();
        let parent = held
            .chain
            .index()
            .get(&on)
            .context("the tip is always indexed")?;
        let network = held.config.network;

        (
            on,
            parent.height + 1,
            held.chain.index().required_bits_after(&on, network)?,
            parent.header.time,
            held.wallet.pubkey_hash(),
            held.mempool.by_fee(),
        )
    };

    if now() <= parent_time {
        return Ok(None);
    }

    let (payments, fees) = fill(&mut entries);
    let reward = subsidy(height)
        .checked_add(fees)
        .context("the subsidy and fees sum past MAX_MONEY")?;

    let coinbase = Transaction::coinbase(
        height,
        0,
        vec![crate::transaction::TxOut {
            value: reward,
            script_pubkey: p2pkh(&address),
        }],
    );

    Ok(Some(Candidate {
        block: Block::new(
            1,
            *on.as_bytes(),
            now(),
            n_bits,
            [vec![coinbase], payments].concat(),
        ),
        on,
    }))
}

/// The header, the coinbase, and the count in front of the transactions. A
/// coinbase's `coinbase_data` is capped at 100 bytes and its outputs are the
/// miner's own, so this is generous rather than tight.
const BLOCK_OVERHEAD: usize = HEADER_SIZE + MAX_COINBASE_DATA + 200;

/// Takes the most valuable transactions that fit. Sizes are measured against
/// what is already in, so the block cannot grow past what a peer will accept.
fn fill(entries: &mut Vec<Entry>) -> (Vec<Transaction>, Amount) {
    let mut room = MAX_BLOCK_SIZE - BLOCK_OVERHEAD;
    let mut payments = Vec::new();
    let mut fees = Amount::ZERO;

    for entry in entries.drain(..) {
        let size = entry.transaction.get_raw_format().len();
        let Some(total) = fees.checked_add(entry.fee) else {
            break;
        };
        if size > room {
            continue;
        }

        room -= size;
        fees = total;
        payments.push(entry.transaction);
    }

    (payments, fees)
}

/// Hashes in bursts, resting between them, and gives up on a candidate the
/// moment the tip moves — there is no point finishing a block on a chain
/// somebody else has already extended.
fn grind(candidate: &mut Candidate, node: &SharedNode, throttle: Throttle) -> Option<Block> {
    let mut from = 0u32;

    loop {
        let until = from.saturating_add(throttle.burst);
        if let Some(nonce) = candidate.block.search(from, until) {
            candidate.block.nonce = nonce;
            candidate.block.seal().ok()?;
            return Some(candidate.block.clone());
        }

        thread::sleep(throttle.rest);
        if node.lock().expect("node lock poisoned").chain.tip() != candidate.on {
            return None;
        }

        if until == u32::MAX {
            // Every nonce tried. The extranonce is the next search space, and
            // building a fresh candidate is what picks it up.
            return None;
        }
        from = until;
    }
}

fn submit(node: &SharedNode, block: Block) -> Result<()> {
    let hash = block.header()?.hash();

    let outcome = {
        let mut held = node.lock().expect("node lock poisoned");
        let network = held.config.network;
        let crate::node::Node {
            chain,
            utxo,
            mempool,
            ..
        } = &mut *held;

        chain.accept(block, utxo, mempool, now(), network)
    };

    match outcome? {
        Accepted::Extended(_) => record(node, format!("Mined {hash}")),
        other => record(
            node,
            format!("Mined {hash}, which the chain took as {other:?}"),
        ),
    }

    announce(node, hash)
}

/// To every Ready peer. There is nobody to leave out — we made it.
fn announce(node: &SharedNode, hash: BlockHash) -> Result<()> {
    let network = node.lock().expect("node lock poisoned").config.network;
    let offer =
        Message::new(Inventory::offered(vec![Item::Block(hash)]), network)?.get_raw_format()?;

    node.lock()
        .expect("node lock poisoned")
        .peers
        .relay(&offer, None);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::crypto::PrivateKey;
    use crate::node::Node;
    use crate::params::TESTNET;
    use crate::validation::fixtures::{funded, pay_to, signed};
    use crate::validation::{check_block, MAX_TRANSACTION_SIZE};
    use crate::wallet::Wallet;

    /// `Node::shared` seeds the set from genesis, so the allocation is there
    /// for the taking.
    fn a_mining_node() -> SharedNode {
        let genesis = TESTNET.genesis().unwrap();
        Node::shared(
            Config {
                api_address: None,
                data_dir: std::path::PathBuf::new(),
                mine: true,
                network: &TESTNET,
                host_address: "127.0.0.1:0".parse().unwrap(),
                addresses_to_connect: Vec::new(),
            },
            &genesis,
            Wallet::new(),
        )
        .unwrap()
    }

    /// Funded with `1_000 + fee` so two entries never share a funding txid,
    /// and paying out 1,000 so the fee is the fee.
    fn an_entry(fee: u64, key: &PrivateKey, node: &SharedNode) -> Entry {
        let mut held = node.lock().unwrap();
        let outpoint = funded(&mut held.utxo, key, 1_000 + fee, 0);

        Entry {
            transaction: signed(key, &[outpoint], vec![pay_to(key, 1_000)]),
            fee: Amount::from_atoms(fee).unwrap(),
        }
    }

    #[test]
    fn a_mined_block_is_one_the_node_would_accept_from_a_stranger() {
        let node = a_mining_node();
        let mut candidate = build(&node).unwrap().unwrap();

        let nonce = candidate.block.search(0, u32::MAX).expect("a nonce exists");
        candidate.block.nonce = nonce;
        candidate.block.seal().unwrap();

        let held = node.lock().unwrap();
        check_block(
            &candidate.block,
            held.chain.index(),
            &held.utxo,
            now(),
            &TESTNET,
        )
        .expect("the miner's own rules are the node's rules");
    }

    #[test]
    fn a_miner_pays_itself_the_subsidy_and_the_fees_it_collected() {
        let node = a_mining_node();
        let key = PrivateKey::random();
        let entries = vec![an_entry(100, &key, &node), an_entry(40, &key, &node)];
        {
            let mut held = node.lock().unwrap();
            let crate::node::Node { mempool, utxo, .. } = &mut *held;
            for entry in entries {
                mempool
                    .accept(entry.transaction, utxo, 1, &TESTNET)
                    .unwrap();
            }
        }

        let candidate = build(&node).unwrap().unwrap();
        let coinbase = &candidate.block.transactions[0];

        assert_eq!(candidate.block.transactions.len(), 3);
        assert_eq!(
            coinbase.outputs[0].value,
            subsidy(1)
                .checked_add(Amount::from_atoms(140).unwrap())
                .unwrap()
        );
        assert!(node
            .lock()
            .unwrap()
            .wallet
            .owns(&coinbase.outputs[0].script_pubkey));
    }

    #[test]
    fn a_miner_takes_the_most_valuable_transactions_first() {
        let node = a_mining_node();
        let key = PrivateKey::random();
        let mut entries = vec![
            an_entry(10, &key, &node),
            an_entry(500, &key, &node),
            an_entry(90, &key, &node),
        ];
        entries.sort_by(|left, right| right.fee.cmp(&left.fee));

        let (payments, fees) = fill(&mut entries.clone());

        assert_eq!(fees, Amount::from_atoms(600).unwrap());
        assert_eq!(payments[0], entries[0].transaction, "the richest first");
    }

    #[test]
    fn a_miner_stops_filling_a_block_that_is_full() {
        let node = a_mining_node();
        let key = PrivateKey::random();
        let mut entry = an_entry(10, &key, &node);
        entry.transaction.outputs[0].script_pubkey = vec![0; MAX_TRANSACTION_SIZE - 200];
        let fat = entry.transaction.get_raw_format().len();
        let mut entries = vec![entry; MAX_BLOCK_SIZE / fat + 2];

        let (payments, _) = fill(&mut entries);

        assert!(
            payments.len() * fat <= MAX_BLOCK_SIZE,
            "{} transactions of {fat} bytes",
            payments.len()
        );
    }

    #[test]
    fn a_miner_abandons_a_candidate_once_the_tip_has_moved() {
        let node = a_mining_node();
        let mut candidate = build(&node).unwrap().unwrap();
        // Bitcoin's mainnet difficulty, so a burst of one will not solve it.
        candidate.block.n_bits = 0x1d00ffff;
        candidate.on = crate::block::BlockHash::from_bytes([9; 32]);

        let abandoned = grind(
            &mut candidate,
            &node,
            Throttle {
                burst: 1,
                rest: Duration::from_millis(1),
            },
        );

        assert!(
            abandoned.is_none(),
            "no point finishing a block nobody wants"
        );
    }

    /// A block's timestamp is the clock's, not the chain's. Mining on a tip we
    /// have not reached would claim a time that has not happened, and enough
    /// of those in a row is a chain every other node refuses.
    #[test]
    fn a_miner_waits_for_the_clock_rather_than_stamping_a_block_ahead_of_it() {
        let node = a_mining_node();
        assert!(
            build(&node).unwrap().is_some(),
            "genesis is dated in the past, so there is work to do"
        );

        // Mine forward until the chain catches up with the wall clock, which
        // on a network wanting a block a second takes at most a second.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let Some(mut candidate) = build(&node).unwrap() else {
                return;
            };
            let nonce = candidate.block.search(0, u32::MAX).unwrap();
            candidate.block.nonce = nonce;
            candidate.block.seal().unwrap();
            submit(&node, candidate.block).unwrap();
        }

        panic!("the miner never ran out of clock to spend");
    }
}
