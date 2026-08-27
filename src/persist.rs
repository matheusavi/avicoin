use crate::block::{Block, BlockHash, Header};
use crate::block_storage::BlockFiles;
use crate::data_dir::DataDir;
use crate::params::Network;
use crate::store::{parse_undo, raw_undo, Batch, Indexed, Store};
use crate::transaction::{Outpoint, Transaction};
use crate::utxo::{Coin, Undo, UtxoSet};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Mutex;

/// Where a block's bytes and its undo record live. Absent until the block is
/// applied — a header is recorded the moment the node learns of it, and there
/// is nothing to point at yet.
#[derive(Clone, Copy, Debug, Default)]
struct Offsets {
    block_at: Option<u64>,
    undo_at: Option<u64>,
}

/// What the node had when it stopped.
pub struct Restored {
    pub headers: Vec<Header>,
    pub utxo: UtxoSet,
    /// How far the UTXO set was advanced. Ordinarily behind the index tip,
    /// because headers arrive ahead of bodies.
    pub best_block: Option<BlockHash>,
}

/// The chain on disk, and the order things reach it.
///
/// **The ordering is the whole of this type.** A block's bytes and its undo
/// record reach their files, and are flushed, *before* the single `redb`
/// transaction that records the block, moves the coins and advances the
/// best-block marker. Every crash window therefore leaves the node at the old
/// state or the new one:
///
/// - between the files and the commit: the files hold bytes nothing points at,
///   which cost disk and nothing else;
/// - inside the commit: redb is atomic, so it did not happen.
///
/// The marker moves with the coins because they are the same commit. A node
/// coming back is at a block boundary, and reaches its best tip by connecting
/// forward from there — the ordinary path, not a recovery one.
///
/// [ADR-0013](../docs/adr/0013-persistence.md) is the decision;
/// [on-disk-format.md](../docs/on-disk-format.md) is the layout.
pub struct Storage {
    files: Mutex<BlockFiles>,
    store: Store,
    offsets: HashMap<BlockHash, Offsets>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Storage({} blocks indexed)", self.offsets.len())
    }
}

impl Storage {
    pub fn open(directory: &DataDir, network: Network) -> Result<Storage> {
        Ok(Storage {
            files: Mutex::new(BlockFiles::open(directory, network)?),
            store: Store::open(directory)?,
            offsets: HashMap::new(),
        })
    }

    /// Loads, rather than replays: the index comes from the store and the UTXO
    /// set is already materialised, so the cost is the size of what is held
    /// rather than the height of the chain.
    pub fn restored(&mut self) -> Result<Restored> {
        let indexed = self.store.headers()?;
        let mut headers = Vec::with_capacity(indexed.len());

        for entry in indexed {
            self.offsets.insert(
                entry.header.hash(),
                Offsets {
                    block_at: entry.block_at,
                    undo_at: entry.undo_at,
                },
            );
            headers.push(entry.header);
        }

        Ok(Restored {
            headers,
            utxo: UtxoSet::restored(self.store.coins()?),
            best_block: self.store.best_block()?,
        })
    }

    /// The genesis block, on a directory that holds nothing yet: its header,
    /// its allocation, and the marker. A store with no headers is a new node
    /// rather than a corrupt one, and this is what makes that true.
    pub fn begin(&mut self, header: &Header, genesis: &Block, utxo: &UtxoSet) -> Result<()> {
        let hash = header.hash();
        let offsets = self.write_files(genesis, &[Vec::new()])?;

        let batch = self.store.batch()?;
        batch.put_header(&indexed(header, offsets))?;
        for (outpoint, coin) in utxo.coins() {
            batch.put_coin(&outpoint, &coin)?;
        }
        batch.mark_best(&hash)?;
        batch.commit()?;

        self.offsets.insert(hash, offsets);
        Ok(())
    }

    pub fn knows(&self, hash: &BlockHash) -> bool {
        self.offsets
            .get(hash)
            .is_some_and(|offsets| offsets.block_at.is_some())
    }

    /// A header the node has accepted but whose block it has not applied.
    pub fn remember_header(&mut self, header: &Header) -> Result<()> {
        let hash = header.hash();
        let offsets = *self.offsets.entry(hash).or_default();

        let batch = self.store.batch()?;
        batch.put_header(&indexed(header, offsets))?;
        batch.commit()
    }

    /// A block that has been applied: its bytes, its undo record, its coins
    /// and the marker. The files first and flushed, then one commit.
    pub fn remember_block(
        &mut self,
        header: &Header,
        block: &Block,
        spent: &[Undo],
        height: u32,
    ) -> Result<()> {
        let hash = header.hash();
        let offsets = self.write_files(block, spent)?;

        let batch = self.store.batch()?;
        batch.put_header(&indexed(header, offsets))?;
        connected(&batch, &block.transactions, spent, height)?;
        batch.mark_best(&hash)?;
        batch.commit()?;

        self.offsets.insert(hash, offsets);
        Ok(())
    }

    /// The same commit in reverse, and the marker back to the parent. Nothing
    /// is written to the files: the block is still there and may be connected
    /// again.
    pub fn remember_disconnect(
        &self,
        parent: &BlockHash,
        block: &Block,
        spent: &[Undo],
    ) -> Result<()> {
        let batch = self.store.batch()?;
        disconnected(&batch, &block.transactions, spent)?;
        batch.mark_best(parent)?;
        batch.commit()
    }

    pub fn block(&self, hash: &BlockHash) -> Result<Option<Block>> {
        let Some(at) = self.offsets.get(hash).and_then(|o| o.block_at) else {
            return Ok(None);
        };

        let raw = self
            .files
            .lock()
            .expect("block files poisoned")
            .blocks
            .read_at(at)?;
        Block::parse_raw(raw)
            .map(Some)
            .with_context(|| format!("reading {hash} back from blocks.dat"))
    }

    pub fn undo(&self, hash: &BlockHash) -> Result<Option<Vec<Undo>>> {
        let Some(at) = self.offsets.get(hash).and_then(|o| o.undo_at) else {
            return Ok(None);
        };

        let raw = self
            .files
            .lock()
            .expect("block files poisoned")
            .undo
            .read_at(at)?;
        parse_undo(&raw)
            .map(Some)
            .with_context(|| format!("reading {hash}'s undo record back"))
    }

    fn write_files(&self, block: &Block, spent: &[Undo]) -> Result<Offsets> {
        let mut files = self.files.lock().expect("block files poisoned");

        let block_at = files.blocks.append(&block.get_raw_format()?)?;
        let undo_at = files.undo.append(&raw_undo(spent))?;
        // Both, before either is pointed at. A sync that came after the commit
        // would leave the store naming bytes that never reached the disk.
        files.blocks.sync()?;
        files.undo.sync()?;

        Ok(Offsets {
            block_at: Some(block_at),
            undo_at: Some(undo_at),
        })
    }
}

fn indexed(header: &Header, offsets: Offsets) -> Indexed {
    Indexed {
        header: *header,
        block_at: offsets.block_at,
        undo_at: offsets.undo_at,
    }
}

/// The same walk `UtxoSet::connect` makes, in the same order, so a transaction
/// spending what an earlier one in its block created lands the same way here.
fn connected(
    batch: &Batch,
    transactions: &[Transaction],
    spent: &[Undo],
    height: u32,
) -> Result<()> {
    for (transaction, undo) in transactions.iter().zip(spent) {
        for (outpoint, _) in undo {
            batch.remove_coin(outpoint)?;
        }
        for (outpoint, coin) in created(transaction, height) {
            batch.put_coin(&outpoint, &coin)?;
        }
    }

    Ok(())
}

/// And `unwind`'s walk: newest first, so a partial application and a full one
/// come apart the same way.
fn disconnected(batch: &Batch, transactions: &[Transaction], spent: &[Undo]) -> Result<()> {
    for (transaction, undo) in transactions.iter().zip(spent).rev() {
        for (outpoint, _) in created(transaction, 0) {
            batch.remove_coin(&outpoint)?;
        }
        for (outpoint, coin) in undo {
            batch.put_coin(outpoint, coin)?;
        }
    }

    Ok(())
}

fn created(transaction: &Transaction, height: u32) -> Vec<(Outpoint, Coin)> {
    let txid = transaction.get_tx_id();
    let from_coinbase = transaction.is_coinbase();

    transaction
        .outputs
        .iter()
        .enumerate()
        .map(|(index, output)| {
            (
                Outpoint {
                    txid,
                    v_out: index as u32,
                },
                Coin {
                    output: output.clone(),
                    height,
                    from_coinbase,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockchain::{Accepted, Chain};
    use crate::crypto::PrivateKey;
    use crate::mempool::Mempool;
    use crate::params::TESTNET;
    use crate::script::p2pkh;
    use crate::util::hash160;
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let path =
                std::env::temp_dir().join(format!("avicoin-persist-{name}-{}", std::process::id()));
            fs::remove_dir_all(&path).ok();
            Scratch(path)
        }

        fn directory(&self) -> DataDir {
            DataDir::open(&self.0, &TESTNET).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    /// A node, opened and closed the way a process would. Everything that
    /// touches the directory is dropped before the next `open`, because two
    /// nodes cannot share one.
    fn open(scratch: &Scratch) -> (Chain, UtxoSet, DataDir) {
        let directory = scratch.directory();
        let storage = Storage::open(&directory, &TESTNET).unwrap();
        let (chain, utxo) = Chain::open(&TESTNET.genesis().unwrap(), storage).unwrap();
        (chain, utxo, directory)
    }

    fn genesis_hash() -> BlockHash {
        TESTNET.genesis().unwrap().header().unwrap().hash()
    }

    fn a_block_on(chain: &Chain, at: u32) -> Block {
        let key = PrivateKey::random();
        let parent = chain.tip();
        let height = chain.height() + 1;
        let n_bits = chain
            .index()
            .required_bits_after(&parent, &TESTNET)
            .unwrap();
        let coinbase = Transaction::coinbase(
            height,
            0,
            vec![crate::transaction::TxOut {
                value: crate::amount::subsidy(height),
                script_pubkey: p2pkh(&crate::crypto::PubKeyHash::from_bytes(hash160(
                    key.public_key().as_bytes(),
                ))),
            }],
        );

        let mut block = Block::new(1, *parent.as_bytes(), at, n_bits, vec![coinbase]);
        block.nonce = block.search(0, u32::MAX).expect("a nonce exists");
        block.seal().unwrap();
        block
    }

    fn mine_on(chain: &mut Chain, utxo: &mut UtxoSet, mempool: &mut Mempool, at: u32) -> BlockHash {
        let block = a_block_on(chain, at);
        let hash = block.header().unwrap().hash();

        assert_eq!(
            chain
                .accept(block, utxo, mempool, at + 1, &TESTNET)
                .unwrap(),
            Accepted::Extended(hash)
        );
        hash
    }

    /// The milestone's whole point: a node that connected blocks and stopped
    /// comes back at the same tip, with the same coins, having executed
    /// nothing.
    #[test]
    fn a_chain_that_was_connected_comes_back_where_it_was() {
        let scratch = Scratch::new("round-trip");
        let (tip, coins, height) = {
            let (mut chain, mut utxo, _held) = open(&scratch);
            let mut mempool = Mempool::new();
            let at = TESTNET.genesis_time + 1;
            for step in 0..4 {
                mine_on(&mut chain, &mut utxo, &mut mempool, at + step);
            }
            let mut coins = utxo.coins();
            coins.sort_by_key(|(outpoint, _)| outpoint.raw());
            (chain.tip(), coins, chain.height())
        };

        let (reopened, set, _held) = open(&scratch);
        let mut back = set.coins();
        back.sort_by_key(|(outpoint, _)| outpoint.raw());

        assert_eq!(reopened.tip(), tip);
        assert_eq!(reopened.height(), height);
        assert_eq!(back, coins);
    }

    /// A restart empties the body cache. Serving a block, and undoing one,
    /// both have to come off the disk after that.
    #[test]
    fn a_block_and_its_undo_record_read_back_after_a_restart() {
        let scratch = Scratch::new("bodies");
        let mined = {
            let (mut chain, mut utxo, _held) = open(&scratch);
            let mut mempool = Mempool::new();
            mine_on(
                &mut chain,
                &mut utxo,
                &mut mempool,
                TESTNET.genesis_time + 1,
            )
        };

        let (mut reopened, mut set, _held) = open(&scratch);
        let mut mempool = Mempool::new();
        let body = reopened.body(&mined).expect("blocks.dat holds it");

        assert_eq!(body.header().unwrap().hash(), mined);
        assert_eq!(
            reopened
                .disconnect(&mut set, &mut mempool, &TESTNET)
                .unwrap(),
            genesis_hash()
        );
    }

    /// A disconnect commits before it moves the set, so the store and the set
    /// agree across a restart taken right after one.
    #[test]
    fn a_disconnect_survives_a_restart() {
        let scratch = Scratch::new("disconnected");
        {
            let (mut chain, mut utxo, _held) = open(&scratch);
            let mut mempool = Mempool::new();
            let at = TESTNET.genesis_time + 1;
            mine_on(&mut chain, &mut utxo, &mut mempool, at);
            mine_on(&mut chain, &mut utxo, &mut mempool, at + 1);
            chain.disconnect(&mut utxo, &mut mempool, &TESTNET).unwrap();
        }

        let (reopened, set, _held) = open(&scratch);

        assert_eq!(reopened.height(), 1);
        assert_eq!(set.coins().len(), reopened.height() as usize + 3);
    }

    /// The marker sitting behind the index tip is the ordinary state, not a
    /// crash artefact. Disconnecting moves the marker back and leaves the
    /// bodies in `blocks.dat`, so a restart taken there has blocks to connect
    /// forward — and does it with the same code a running node uses.
    #[test]
    fn a_marker_behind_the_index_is_caught_up_from_disk() {
        let scratch = Scratch::new("catch-up");
        let (tip, height) = {
            let (mut chain, mut utxo, _held) = open(&scratch);
            let mut mempool = Mempool::new();
            let at = TESTNET.genesis_time + 1;
            for step in 0..3 {
                mine_on(&mut chain, &mut utxo, &mut mempool, at + step);
            }
            let landed = (chain.tip(), chain.height());

            chain.disconnect(&mut utxo, &mut mempool, &TESTNET).unwrap();
            chain.disconnect(&mut utxo, &mut mempool, &TESTNET).unwrap();
            landed
        };

        let (mut chain, mut utxo, _held) = open(&scratch);
        assert_eq!(
            chain.height(),
            1,
            "the marker is where the disconnects left it"
        );

        let applied = chain
            .catch_up(
                &mut utxo,
                &mut Mempool::new(),
                TESTNET.genesis_time + 100,
                &TESTNET,
            )
            .unwrap();

        assert_eq!(applied, 2);
        assert_eq!(chain.tip(), tip);
        assert_eq!(chain.height(), height);
    }

    /// The other reason the marker lags: a header is committed the moment the
    /// node accepts it, and the block behind it may never arrive. There is
    /// nothing to connect, and startup says so rather than failing.
    #[test]
    fn a_header_whose_block_never_came_leaves_the_tip_alone() {
        let scratch = Scratch::new("header-only");
        {
            let (mut chain, mut utxo, _held) = open(&scratch);
            let mut mempool = Mempool::new();
            let at = TESTNET.genesis_time + 1;
            mine_on(&mut chain, &mut utxo, &mut mempool, at);

            let promised = a_block_on(&chain, at + 1);
            chain
                .add_header(promised.header().unwrap(), at + 2, &TESTNET)
                .unwrap();
        }

        let (mut chain, mut utxo, _held) = open(&scratch);
        let applied = chain
            .catch_up(
                &mut utxo,
                &mut Mempool::new(),
                TESTNET.genesis_time + 100,
                &TESTNET,
            )
            .unwrap();

        assert_eq!(applied, 0);
        assert_eq!(chain.height(), 1);
    }

    /// A marker naming a block the index does not hold cannot be reconciled,
    /// and a node that trusted it would be running on a set it cannot explain.
    #[test]
    fn a_marker_the_index_does_not_hold_is_a_corrupt_store() {
        let scratch = Scratch::new("bad-marker");
        {
            let (mut chain, mut utxo, _held) = open(&scratch);
            mine_on(
                &mut chain,
                &mut utxo,
                &mut Mempool::new(),
                TESTNET.genesis_time + 1,
            );
        }
        {
            let directory = scratch.directory();
            let store = Store::open(&directory).unwrap();
            let batch = store.batch().unwrap();
            batch.mark_best(&BlockHash::from_bytes([9; 32])).unwrap();
            batch.commit().unwrap();
        }

        let directory = scratch.directory();
        let storage = Storage::open(&directory, &TESTNET).unwrap();

        let error = format!(
            "{:#}",
            Chain::open(&TESTNET.genesis().unwrap(), storage).unwrap_err()
        );

        assert!(error.contains("the index does not hold"), "{error}");
    }

    #[test]
    fn a_fresh_directory_starts_at_genesis_and_says_so_on_disk() {
        let scratch = Scratch::new("fresh");
        let genesis = TESTNET.genesis().unwrap();

        let (chain, utxo, _held) = open(&scratch);

        assert_eq!(chain.height(), 0);
        assert_eq!(chain.tip(), genesis_hash());
        assert_eq!(utxo.coins().len(), genesis.transactions[0].outputs.len());
    }
}
