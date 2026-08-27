use crate::block::{Block, BlockHash, Header};
use crate::difficulty::{median_time_past, required_bits, MEDIAN_TIME_SPAN, RETARGET_WINDOW};
use crate::mempool::Mempool;
use crate::params::Network;
use crate::transaction::Transaction;
use crate::utxo::{Undo, UtxoSet};
use crate::validation::{check_block, ClockDrift};
use anyhow::{anyhow, bail, Result};
use primitive_types::U256;
use std::collections::{HashMap, HashSet};

/// What the node knows about one block, whether or not it has connected it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub header: Header,
    pub height: u32,
    /// The work of this block *and every ancestor*. Height is not a proxy for
    /// it once difficulty varies per block, so chain selection by height would
    /// simply be wrong — ADR-0012.
    pub total_work: U256,
    pub parent: Option<BlockHash>,
}

/// Every header the node has accepted, and which of them is the best chain.
/// Tolerates more than one tip: two miners racing is the normal case, not an
/// error, and the node holds both until one branch outweighs the other.
#[derive(Debug)]
pub struct BlockIndex {
    entries: HashMap<BlockHash, Entry>,
    best: BlockHash,
}

impl BlockIndex {
    pub fn new(genesis: Header) -> Result<Self> {
        let hash = genesis.hash();
        let entry = Entry {
            header: genesis,
            height: 0,
            total_work: genesis.work()?,
            parent: None,
        };

        Ok(BlockIndex {
            entries: HashMap::from([(hash, entry)]),
            best: hash,
        })
    }

    /// Records a header whose parent is already known. A header whose parent
    /// is not is refused rather than given height zero — an orphan is held by
    /// the caller, which is the only place that can decide to wait.
    pub fn insert(&mut self, header: Header) -> Result<BlockHash> {
        let hash = header.hash();
        if self.entries.contains_key(&hash) {
            return Ok(hash);
        }

        let parent = self
            .entries
            .get(&header.previous_block_hash)
            .ok_or_else(|| anyhow!("{} has no known parent", header.previous_block_hash))?;

        let entry = Entry {
            height: parent.height + 1,
            total_work: parent
                .total_work
                .checked_add(header.work()?)
                .ok_or_else(|| anyhow!("cumulative work past 2^256"))?,
            parent: Some(header.previous_block_hash),
            header,
        };

        let outweighs = self.is_better(&entry);
        self.entries.insert(hash, entry);
        if outweighs {
            self.best = hash;
        }

        Ok(hash)
    }

    // Strictly greater, so the tip already held keeps its place against an
    // equal branch: first seen wins, and being loud does not.
    fn is_better(&self, candidate: &Entry) -> bool {
        candidate.total_work > self.best().total_work
    }

    pub fn best(&self) -> &Entry {
        self.entries
            .get(&self.best)
            .expect("the best tip is indexed")
    }

    pub fn best_hash(&self) -> BlockHash {
        self.best
    }

    pub fn get(&self, hash: &BlockHash) -> Option<&Entry> {
        self.entries.get(hash)
    }

    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.entries.contains_key(hash)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry no other entry claims as a parent.
    pub fn tips(&self) -> Vec<BlockHash> {
        let claimed: HashSet<BlockHash> = self
            .entries
            .values()
            .filter_map(|entry| entry.parent)
            .collect();

        let mut tips: Vec<BlockHash> = self
            .entries
            .keys()
            .filter(|hash| !claimed.contains(hash))
            .copied()
            .collect();
        tips.sort_unstable();

        tips
    }

    /// Walks back from `hash`, inclusive, at most `count` entries, and returns
    /// them oldest first.
    pub fn ancestry(&self, hash: &BlockHash, count: usize) -> Vec<&Entry> {
        let mut walked = Vec::new();
        let mut at = Some(*hash);

        while walked.len() < count {
            let Some(entry) = at.and_then(|hash| self.entries.get(&hash)) else {
                break;
            };
            walked.push(entry);
            at = entry.parent;
        }

        walked.reverse();
        walked
    }

    fn timestamps_to(&self, hash: &BlockHash, count: usize) -> Vec<u32> {
        self.ancestry(hash, count)
            .into_iter()
            .map(|entry| entry.header.time)
            .collect()
    }

    /// The `n_bits` a child of `parent` must state.
    pub fn required_bits_after(&self, parent: &BlockHash, network: Network) -> Result<u32> {
        let entry = self
            .get(parent)
            .ok_or_else(|| anyhow!("{parent} is not a block this node knows"))?;

        required_bits(
            &self.timestamps_to(parent, RETARGET_WINDOW + 1),
            entry.header.n_bits,
            network,
        )
    }

    /// The median of the blocks up to and including `parent` that
    /// median-time-past looks at.
    pub fn median_time_after(&self, parent: &BlockHash) -> Result<u32> {
        median_time_past(&self.timestamps_to(parent, MEDIAN_TIME_SPAN))
            .ok_or_else(|| anyhow!("{parent} is not a block this node knows"))
    }
}

/// The chain as the node has actually applied it: a tip, the block bodies
/// behind it, and what each of them consumed.
///
/// Separate from the index because knowing a header and having connected it
/// are different things — headers-first sync learns of blocks long before it
/// has their bodies.
#[derive(Debug)]
pub struct Chain {
    index: BlockIndex,
    tip: BlockHash,
    /// Held in memory, and never pruned, so a long-running node's footprint
    /// grows with its chain. [ADR-0013](../docs/adr/0013-persistence.md) is
    /// the answer to both in M5 — until then a node that dies mid-reorg
    /// cannot recover either.
    bodies: HashMap<BlockHash, Block>,
    undo: HashMap<BlockHash, Vec<Undo>>,
    /// Blocks whose *body* failed validation. Their headers stay in the index
    /// — the work is real — but the branch through them is never chosen again,
    /// or a node offered one would retry it forever.
    failed: HashSet<BlockHash>,
    /// Blocks whose parent we have not seen. Out-of-order delivery is a fact
    /// of a network rather than a failure, and re-requesting is worse than
    /// remembering — but a stranger fills this, so it is bounded.
    orphans: HashMap<BlockHash, Block>,
}

/// How many blocks may wait for a parent. Each is at most `MAX_BLOCK_SIZE`.
pub const MAX_ORPHANS: usize = 64;

/// What accepting a block did.
#[derive(Debug, PartialEq, Eq)]
pub enum Accepted {
    /// It built on the tip.
    Extended(BlockHash),
    /// It made another branch heavier, and the node moved to it. `undone` is
    /// how many blocks came off and `applied` how many went on — both bounded
    /// by the depth of the switch, neither by the height of the chain.
    Reorganised {
        to: BlockHash,
        undone: usize,
        applied: usize,
    },
    /// Recorded, but the tip stayed where it was.
    Held(BlockHash),
    /// Its parent is not a block this node knows, so it waits for one.
    Orphaned(BlockHash),
}

impl Chain {
    pub fn new(genesis: &Block) -> Result<Self> {
        let header = genesis.header()?;
        let tip = header.hash();

        Ok(Chain {
            index: BlockIndex::new(header)?,
            tip,
            bodies: HashMap::from([(tip, genesis.clone())]),
            undo: HashMap::from([(tip, Vec::new())]),
            failed: HashSet::new(),
            orphans: HashMap::new(),
        })
    }

    pub fn index(&self) -> &BlockIndex {
        &self.index
    }

    pub fn tip(&self) -> BlockHash {
        self.tip
    }

    pub fn height(&self) -> u32 {
        self.index
            .get(&self.tip)
            .expect("the tip is indexed")
            .height
    }

    pub fn body(&self, hash: &BlockHash) -> Option<&Block> {
        self.bodies.get(hash)
    }

    /// Whether this is a block we already have, connected or waiting — which
    /// is the question an `inv` asks.
    pub fn holds(&self, hash: &BlockHash) -> bool {
        self.bodies.contains_key(hash) || self.orphans.contains_key(hash)
    }

    /// Takes a block from anywhere and decides what it means: an extension of
    /// the tip, a heavier branch worth moving to, or something recorded and
    /// left alone. Chain selection is by cumulative work, never by height.
    pub fn accept(
        &mut self,
        block: Block,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        now: u32,
        network: Network,
    ) -> Result<Accepted> {
        let header = block.header()?;
        let hash = header.hash();
        // Connected, not merely known: a block whose body we hold but have
        // since disconnected is one we may need to apply again.
        if self.undo.contains_key(&hash) {
            return Ok(Accepted::Held(hash));
        }
        // Already tried and refused. Without this, a peer resending it makes
        // us revalidate it every time, which is the work the record exists to
        // save.
        if self.failed.contains(&hash) {
            return Ok(Accepted::Held(hash));
        }

        if !self.index.contains(&header.previous_block_hash) {
            self.orphan(hash, block);
            return Ok(Accepted::Orphaned(hash));
        }

        self.index.insert(header)?;
        self.bodies.insert(hash, block);

        let outcome = if header.previous_block_hash == self.tip {
            self.apply(hash, utxo, mempool, now, network)?;
            Accepted::Extended(hash)
        } else if self.work_of(&hash)? > self.work_of(&self.tip)? && !self.branch_failed(&hash)? {
            let (undone, applied) = self.switch_to(hash, utxo, mempool, now, network)?;
            Accepted::Reorganised {
                to: hash,
                undone,
                applied,
            }
        } else {
            Accepted::Held(hash)
        };

        self.adopt_orphans(utxo, mempool, now, network);

        Ok(outcome)
    }

    fn orphan(&mut self, hash: BlockHash, block: Block) {
        if self.orphans.len() >= MAX_ORPHANS {
            return;
        }

        self.orphans.insert(hash, block);
    }

    /// Any orphan whose parent has just arrived — and any orphan of *those*,
    /// since a whole branch can come in backwards.
    fn adopt_orphans(
        &mut self,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        now: u32,
        network: Network,
    ) {
        loop {
            let ready: Vec<BlockHash> = self
                .orphans
                .iter()
                .filter(|(_, block)| {
                    block
                        .header()
                        .is_ok_and(|header| self.index.contains(&header.previous_block_hash))
                })
                .map(|(hash, _)| *hash)
                .collect();

            if ready.is_empty() {
                return;
            }

            for hash in ready {
                let Some(block) = self.orphans.remove(&hash) else {
                    continue;
                };
                let _ = self.accept(block, utxo, mempool, now, network);
            }
        }
    }

    /// Validates a block against the current tip and applies it.
    pub fn connect(
        &mut self,
        block: Block,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        now: u32,
        network: Network,
    ) -> Result<BlockHash> {
        let header = block.header()?;
        if header.previous_block_hash != self.tip {
            bail!(
                "{} builds on {}, not on the tip {}",
                header.hash(),
                header.previous_block_hash,
                self.tip
            );
        }

        match self.accept(block, utxo, mempool, now, network)? {
            Accepted::Extended(hash) => Ok(hash),
            other => bail!("a block on the tip should extend it, not {other:?}"),
        }
    }

    /// Whether anything between `target` and the fork has already failed. A
    /// branch through a block that cannot connect is one that never will, and
    /// checking only the tip lets a peer make us walk the whole branch again
    /// for the price of one more mined header.
    fn branch_failed(&self, target: &BlockHash) -> Result<bool> {
        let fork = self.fork_point(&self.tip, target)?;
        let climb = (self.expect(target)?.height - self.expect(&fork)?.height) as usize;

        Ok(self
            .index
            .ancestry(target, climb)
            .into_iter()
            .any(|entry| self.failed.contains(&entry.header.hash())))
    }

    fn work_of(&self, hash: &BlockHash) -> Result<U256> {
        Ok(self.expect(hash)?.total_work)
    }

    fn expect(&self, hash: &BlockHash) -> Result<&Entry> {
        self.index
            .get(hash)
            .ok_or_else(|| anyhow!("{hash} is not a block this node knows"))
    }

    /// Applies a block whose header is already indexed and whose body is
    /// already held, and whose parent is the tip. Nothing moves until
    /// validation has passed — and the index was written before this was
    /// called — so a refusal leaves the set and the tip exactly as they were.
    fn apply(
        &mut self,
        hash: BlockHash,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        now: u32,
        network: Network,
    ) -> Result<()> {
        let block = self
            .bodies
            .get(&hash)
            .ok_or_else(|| anyhow!("{hash} has no body to apply"))?
            .clone();

        if let Err(refusal) = check_block(&block, &self.index, utxo, now, network) {
            // A block ahead of our clock is not invalid, it is early — the
            // same bytes are fine a minute later. Recording it would refuse it
            // forever, which is the shape of mistake #73 is about.
            if refusal.downcast_ref::<ClockDrift>().is_some() {
                self.bodies.remove(&hash);
                return Err(refusal);
            }

            // Recorded so the branch is not chosen again. #73 carves out the
            // rules where a *valid* block can share this hash.
            self.failed.insert(hash);
            // The header stays — its work is real, and forgetting it would
            // have us fetch the block again — but the body goes. A peer that
            // mines a valid header over an invalid body costs us a hash and
            // thirty-two bytes, not a megabyte.
            self.bodies.remove(&hash);
            return Err(refusal);
        }

        let height = self.height() + 1;
        let mut spent = Vec::new();
        for transaction in &block.transactions {
            match utxo.connect(transaction, height) {
                Ok(undo) => spent.push(undo),
                Err(broken) => {
                    // Unreachable if validation and application agree, which
                    // is exactly why it is worth not assuming.
                    unwind(utxo, &block.transactions, &spent);
                    return Err(broken.context("applying a block validation accepted"));
                }
            }
        }

        for transaction in &block.transactions {
            mempool.remove(&transaction.get_tx_id());
        }

        self.undo.insert(hash, spent);
        self.tip = hash;

        Ok(())
    }

    /// Walks back to where the two branches last agreed, then forward along
    /// the new one. Cost is proportional to the *depth* of the switch, not to
    /// the height of the chain — ADR-0012's whole reason for undo records.
    ///
    /// A block on the new branch that fails puts the node back where it was.
    fn switch_to(
        &mut self,
        target: BlockHash,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        now: u32,
        network: Network,
    ) -> Result<(usize, usize)> {
        let fork = self.fork_point(&self.tip, &target)?;
        let undone = self.rewind_to(fork, utxo, mempool, network)?;

        let climb = (self.expect(&target)?.height - self.expect(&fork)?.height) as usize;
        let forward: Vec<BlockHash> = self
            .index
            .ancestry(&target, climb)
            .into_iter()
            .map(|entry| entry.header.hash())
            .collect();

        for hash in &forward {
            if let Err(refusal) = self.apply(*hash, utxo, mempool, now, network) {
                self.retreat(&undone, fork, utxo, mempool, now, network);
                return Err(refusal.context("the heavier branch does not validate"));
            }
        }

        Ok((undone.len(), forward.len()))
    }

    /// Back to where we started, after a branch turned out not to validate.
    fn retreat(
        &mut self,
        undone: &[BlockHash],
        fork: BlockHash,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        now: u32,
        network: Network,
    ) {
        self.rewind_to(fork, utxo, mempool, network)
            .expect("undoing what was just applied");

        for hash in undone.iter().rev() {
            self.apply(*hash, utxo, mempool, now, network)
                .expect("reapplying a branch that was connected a moment ago");
        }
    }

    /// Disconnects back to `fork`, returning what came off, newest first.
    fn rewind_to(
        &mut self,
        fork: BlockHash,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        network: Network,
    ) -> Result<Vec<BlockHash>> {
        let mut undone = Vec::new();

        while self.tip != fork {
            undone.push(self.tip);
            self.disconnect(utxo, mempool, network)?;
        }

        Ok(undone)
    }

    /// Where two branches last agreed.
    fn fork_point(&self, from: &BlockHash, to: &BlockHash) -> Result<BlockHash> {
        let (mut left, mut right) = (*from, *to);

        loop {
            if left == right {
                return Ok(left);
            }

            let (a, b) = (self.expect(&left)?, self.expect(&right)?);
            let no_fork = || anyhow!("two branches with no common ancestor");

            if a.height >= b.height {
                left = a.parent.ok_or_else(no_fork)?;
            } else {
                right = b.parent.ok_or_else(no_fork)?;
            }
        }
    }

    /// Puts the tip back, restoring what it consumed and returning its
    /// payments to the mempool.
    pub fn disconnect(
        &mut self,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        network: Network,
    ) -> Result<BlockHash> {
        let entry = self.index.get(&self.tip).expect("the tip is indexed");
        let parent = entry
            .parent
            .ok_or_else(|| anyhow!("genesis is not a block to disconnect"))?;

        let block = self
            .bodies
            .get(&self.tip)
            .ok_or_else(|| anyhow!("{} has no body to undo", self.tip))?
            .clone();
        let spent = self
            .undo
            .get(&self.tip)
            .ok_or_else(|| anyhow!("{} has no undo record", self.tip))?
            .clone();

        unwind(utxo, &block.transactions, &spent);
        self.undo.remove(&self.tip);
        self.tip = parent;

        // Everything but the coinbase goes back, and only what is still valid
        // against the set as it now stands. A refusal here is the ordinary
        // case — a payment that depended on the block is not one to keep — so
        // it is dropped rather than reported. There is no logging seam in this
        // module to report it through.
        let height = self.height() + 1;
        for transaction in block.transactions.into_iter().skip(1) {
            let _ = mempool.accept(transaction, utxo, height, network);
        }

        Ok(parent)
    }
}

/// Reverses what `spent` records, newest first, so a partial application and a
/// full one are undone by the same code.
fn unwind(utxo: &mut UtxoSet, transactions: &[Transaction], spent: &[Undo]) {
    for (transaction, undo) in transactions.iter().zip(spent).rev() {
        utxo.disconnect(transaction, undo)
            .expect("undoing exactly what was applied");
    }
}

#[cfg(test)]
mod chain_tests {
    use super::*;
    use crate::amount::{subsidy, Amount};
    use crate::crypto::PrivateKey;
    use crate::params::TESTNET;
    use crate::transaction::Outpoint;
    use crate::utxo::UtxoView;
    use crate::validation::check_spend;
    use crate::validation::fixtures::{funded, pay_to, signed};

    const TARGET_BLOCK_TIME: u32 = TESTNET.target_block_time;

    struct Node {
        chain: Chain,
        utxo: UtxoSet,
        mempool: Mempool,
        now: u32,
    }

    fn a_node() -> Node {
        let genesis = TESTNET.genesis().unwrap();
        let mut utxo = UtxoSet::new();
        utxo.connect(&genesis.transactions[0], 0).unwrap();
        let now = genesis.time + 1_000 * TARGET_BLOCK_TIME;

        Node {
            chain: Chain::new(&genesis).unwrap(),
            utxo,
            mempool: Mempool::new(),
            now,
        }
    }

    impl Node {
        fn candidate(&self, payments: Vec<Transaction>) -> Block {
            let parent = self.chain.index().get(&self.chain.tip()).unwrap();
            let height = parent.height + 1;

            let mut view = UtxoView::over(&self.utxo);
            let mut fees = Amount::ZERO;
            for payment in &payments {
                fees = fees
                    .checked_add(check_spend(payment, &view, height, &TESTNET).unwrap())
                    .unwrap();
                view.apply(payment, height).unwrap();
            }

            let owed = subsidy(height).checked_add(fees).unwrap();
            let coinbase = Transaction::coinbase(
                height,
                height as u64,
                vec![pay_to(&PrivateKey::random(), owed.atoms())],
            );

            let mut block = Block::new(
                1,
                *parent.header.hash().as_bytes(),
                parent.header.time + TARGET_BLOCK_TIME,
                self.chain
                    .index()
                    .required_bits_after(&self.chain.tip(), &TESTNET)
                    .unwrap(),
                [vec![coinbase], payments].concat(),
            );
            assert!(block.mine().unwrap());

            block
        }

        fn connect(&mut self, block: Block) -> Result<BlockHash> {
            self.chain
                .connect(block, &mut self.utxo, &mut self.mempool, self.now, &TESTNET)
        }

        fn disconnect(&mut self) -> Result<BlockHash> {
            self.chain
                .disconnect(&mut self.utxo, &mut self.mempool, &TESTNET)
        }

        fn coins(&self) -> Vec<(Outpoint, crate::utxo::Coin)> {
            let mut coins = self.utxo.coins();
            coins.sort_by_key(|(outpoint, _)| (outpoint.txid.to_string(), outpoint.v_out));
            coins
        }
    }

    #[test]
    fn connecting_a_block_advances_the_tip_and_pays_its_miner() {
        let mut node = a_node();
        let before = node.utxo.len();

        let block = node.candidate(Vec::new());
        let hash = node.connect(block).unwrap();

        assert_eq!(node.chain.tip(), hash);
        assert_eq!(node.chain.height(), 1);
        assert_eq!(node.utxo.len(), before + 1, "the coinbase's one output");
    }

    #[test]
    fn a_block_that_does_not_build_on_the_tip_is_refused() {
        let mut node = a_node();
        let first = node.candidate(Vec::new());
        let sibling = node.candidate(Vec::new());
        node.connect(first).unwrap();

        assert!(node.connect(sibling).is_err(), "it builds on genesis");
    }

    #[test]
    fn a_block_that_fails_validation_leaves_the_tip_and_the_set_alone() {
        let mut node = a_node();
        let before = node.coins();
        let tip = node.chain.tip();

        let mut broken = node.candidate(Vec::new());
        broken.nonce = broken.nonce.wrapping_add(1);

        assert!(node.connect(broken).is_err());
        assert_eq!(node.chain.tip(), tip);
        assert_eq!(node.coins(), before);
    }

    #[test]
    fn disconnecting_restores_the_set_exactly() {
        let mut node = a_node();
        let key = PrivateKey::random();
        let outpoint = funded(&mut node.utxo, &key, 1_000, 0);
        let before = node.coins();

        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let block = node.candidate(vec![payment]);
        node.connect(block).unwrap();
        assert_ne!(node.coins(), before);

        node.disconnect().unwrap();

        assert_eq!(node.coins(), before);
        assert_eq!(node.chain.height(), 0);
    }

    #[test]
    fn connect_disconnect_connect_lands_where_connect_alone_does() {
        let mut node = a_node();
        let block = node.candidate(Vec::new());

        node.connect(block.clone()).unwrap();
        let once = node.coins();

        node.disconnect().unwrap();
        node.connect(block).unwrap();

        assert_eq!(node.coins(), once);
        assert_eq!(node.chain.height(), 1);
    }

    #[test]
    fn a_connected_blocks_payments_leave_the_mempool() {
        let mut node = a_node();
        let key = PrivateKey::random();
        let outpoint = funded(&mut node.utxo, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let txid = node
            .mempool
            .accept(payment.clone(), &node.utxo, 1, &TESTNET)
            .unwrap();

        let block = node.candidate(vec![payment]);
        node.connect(block).unwrap();

        assert!(!node.mempool.contains(&txid));
    }

    #[test]
    fn a_disconnected_blocks_payments_come_back_to_the_mempool() {
        let mut node = a_node();
        let key = PrivateKey::random();
        let outpoint = funded(&mut node.utxo, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let txid = payment.get_tx_id();

        let block = node.candidate(vec![payment]);
        node.connect(block).unwrap();
        assert!(node.mempool.is_empty());

        node.disconnect().unwrap();

        assert!(
            node.mempool.contains(&txid),
            "the payment is unconfirmed again"
        );
    }

    #[test]
    fn a_disconnected_coinbase_does_not_come_back_to_the_mempool() {
        let mut node = a_node();
        let block = node.candidate(Vec::new());
        node.connect(block).unwrap();

        node.disconnect().unwrap();

        assert!(
            node.mempool.is_empty(),
            "a block creates a coinbase; a peer does not"
        );
    }

    /// The test network matures a coinbase in one block, so a coinbase mined
    /// at height 1 is spendable at height 2 and not before. Disconnecting the
    /// block that spent it has to restore it as a coinbase at height 1 — the
    /// two fields ADR-0012 says the undo record cannot do without.
    #[test]
    fn a_restored_coinbase_output_is_immature_against_the_tip_it_comes_back_to() {
        let mut node = a_node();
        let first = node.candidate(Vec::new());
        let hash = node.connect(first).unwrap();
        let mined = node.chain.body(&hash).unwrap().transactions[0].clone();
        let reward = Outpoint {
            txid: mined.get_tx_id(),
            v_out: 0,
        };

        let restored = node.utxo.get(&reward).unwrap();
        assert!(restored.from_coinbase && restored.height == 1);
        assert!(!restored.spendable_at(1, TESTNET.maturity), "too soon");
        assert!(
            restored.spendable_at(2, TESTNET.maturity),
            "one block later"
        );

        node.disconnect().unwrap();

        assert!(
            node.utxo.get(&reward).is_none(),
            "the coin the block created goes with the block"
        );
    }

    #[test]
    fn a_payment_that_no_longer_validates_does_not_come_back_to_the_mempool() {
        let mut node = a_node();
        let key = PrivateKey::random();
        let outpoint = funded(&mut node.utxo, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let txid = payment.get_tx_id();

        let block = node.candidate(vec![payment]);
        node.connect(block).unwrap();

        // The coin it spent is gone by the time it would come back, so the
        // payment is no longer a payment.
        let restored = node.utxo.get(&outpoint);
        assert!(
            restored.is_none(),
            "spent by the block we are about to undo"
        );
        node.utxo
            .connect(&Transaction::coinbase(9, 9, vec![pay_to(&key, 5)]), 0)
            .unwrap();
        node.chain.undo.get_mut(&node.chain.tip()).unwrap()[1].clear();

        node.disconnect().unwrap();

        assert!(
            !node.mempool.contains(&txid),
            "it spends an outpoint nothing restored"
        );
    }

    impl Node {
        /// A block on top of `parent`, without connecting it. `seed` only
        /// makes two candidates at the same height differ.
        ///
        /// Every block here sits at the test network's floor and arrives
        /// exactly on target, so the retarget rule always asks for
        /// `starting_bits` — which is why this can build a branch the index
        /// has never seen.
        fn candidate_on(&self, parent: BlockHash, payments: Vec<Transaction>, seed: u64) -> Block {
            let entry = self.chain.index().get(&parent).unwrap();

            self.candidate_after(entry.header, entry.height, payments, seed)
        }

        fn candidate_after(
            &self,
            parent: crate::block::Header,
            parent_height: u32,
            payments: Vec<Transaction>,
            seed: u64,
        ) -> Block {
            let height = parent_height + 1;
            let coinbase = Transaction::coinbase(
                height,
                seed,
                vec![pay_to(&PrivateKey::random(), subsidy(height).atoms())],
            );

            let mut block = Block::new(
                1,
                *parent.hash().as_bytes(),
                parent.time + TARGET_BLOCK_TIME,
                TESTNET.starting_bits,
                [vec![coinbase], payments].concat(),
            );
            assert!(block.mine().unwrap());

            block
        }

        fn accept(&mut self, block: Block) -> Result<Accepted> {
            self.chain
                .accept(block, &mut self.utxo, &mut self.mempool, self.now, &TESTNET)
        }

        /// Extends `from` by `count` blocks without connecting them, and
        /// returns them oldest first.
        fn branch(&mut self, from: BlockHash, count: u32, seed: u64) -> Vec<Block> {
            let entry = self.chain.index().get(&from).unwrap();
            let (mut header, mut height) = (entry.header, entry.height);
            let mut blocks = Vec::new();

            for step in 0..count as u64 {
                let block = self.candidate_after(header, height, Vec::new(), seed * 100 + step);
                header = block.header().unwrap();
                height += 1;
                blocks.push(block);
            }

            blocks
        }
    }

    #[test]
    fn a_heavier_branch_takes_the_tip() {
        let mut node = a_node();
        let root = node.chain.tip();
        let short = node.branch(root, 1, 1);
        let long = node.branch(root, 2, 2);

        node.accept(short[0].clone()).unwrap();
        assert_eq!(node.chain.height(), 1);

        node.accept(long[0].clone()).unwrap();
        let switched = node.accept(long[1].clone()).unwrap();

        assert_eq!(
            switched,
            Accepted::Reorganised {
                to: long[1].header().unwrap().hash(),
                undone: 1,
                applied: 2,
            }
        );
        assert_eq!(node.chain.tip(), long[1].header().unwrap().hash());
        assert_eq!(node.chain.height(), 2);
    }

    #[test]
    fn an_equal_branch_does_not_take_the_tip() {
        let mut node = a_node();
        let root = node.chain.tip();
        let first = node.branch(root, 1, 1);
        let second = node.branch(root, 1, 2);
        node.accept(first[0].clone()).unwrap();

        let held = node.accept(second[0].clone()).unwrap();

        assert_eq!(held, Accepted::Held(second[0].header().unwrap().hash()));
        assert_eq!(node.chain.tip(), first[0].header().unwrap().hash());
    }

    #[test]
    fn a_switch_lands_where_connecting_that_branch_alone_would() {
        let mut node = a_node();
        let root = node.chain.tip();
        let losing = node.branch(root, 1, 1);
        let winning = node.branch(root, 2, 2);

        let mut straight = a_node();
        for block in &winning {
            straight.accept(block.clone()).unwrap();
        }

        node.accept(losing[0].clone()).unwrap();
        for block in &winning {
            node.accept(block.clone()).unwrap();
        }

        assert_eq!(node.chain.tip(), straight.chain.tip());
        assert_eq!(node.coins(), straight.coins());
    }

    #[test]
    fn a_branch_that_does_not_validate_leaves_the_node_where_it_was() {
        let mut node = a_node();
        let root = node.chain.tip();
        // Two blocks held, three offered: the switch is attempted at the third,
        // which is the one that fails.
        let held = node.branch(root, 2, 1);
        for block in &held {
            node.accept(block.clone()).unwrap();
        }
        let tip = node.chain.tip();
        let coins = node.coins();

        let mut rival = node.branch(root, 3, 2);
        // The third block pays itself more than it earned, and is re-mined so
        // proof-of-work is not what refuses it.
        rival[2].transactions[0].outputs[0].value =
            Amount::from_atoms(subsidy(3).atoms() + 1).unwrap();
        assert!(rival[2].mine().unwrap());

        node.accept(rival[0].clone()).unwrap();
        node.accept(rival[1].clone()).unwrap();
        let refused = node.accept(rival[2].clone());

        assert!(refused.is_err(), "the branch does not validate");
        assert_eq!(node.chain.tip(), tip, "and the node is where it started");
        assert_eq!(node.coins(), coins);
    }

    /// Every branch here is at the same difficulty, so heavier and longer
    /// coincide. This one is not: two blocks at a harder target outweigh
    /// three easy ones, and height would choose the other way.
    #[test]
    fn a_shorter_branch_wins_the_switch_when_it_carries_more_work() {
        let mut node = a_node();
        let root = node.chain.tip();
        let easy = node.branch(root, 3, 1);
        for block in &easy {
            node.accept(block.clone()).unwrap();
        }

        let entry = node.chain.index().get(&root).unwrap();
        let (mut header, mut height) = (entry.header, entry.height);
        let mut hard = Vec::new();
        for step in 0..2u64 {
            let mut block = node.candidate_after(header, height, Vec::new(), 500 + step);
            // One byte of exponent harder than the network floor, so two of
            // these outweigh three of the others.
            block.n_bits = 0x1f00ffff;
            assert!(block.mine().unwrap());
            header = block.header().unwrap();
            height += 1;
            hard.push(block);
        }

        for block in &hard {
            let _ = node.accept(block.clone());
        }

        assert_eq!(
            node.chain.index().best_hash(),
            hard[1].header().unwrap().hash(),
            "the index picks the heavier branch, not the longer one"
        );
    }

    #[test]
    fn a_reorg_moves_the_mempool_both_ways() {
        let mut node = a_node();
        let key = PrivateKey::random();
        let outpoint = funded(&mut node.utxo, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let txid = payment.get_tx_id();
        let root = node.chain.tip();

        let carrying = node.candidate_on(root, vec![payment], 1);
        node.accept(carrying).unwrap();
        assert!(!node.mempool.contains(&txid), "confirmed");

        let rival = node.branch(root, 2, 2);
        for block in rival {
            node.accept(block).unwrap();
        }

        assert!(
            node.mempool.contains(&txid),
            "orphaned by the switch, so unconfirmed again"
        );

        // And the other way: a branch that confirms it takes it back out.
        let entry = node.chain.index().get(&node.chain.tip()).unwrap();
        let (header, height) = (entry.header, entry.height);
        let carried = node.candidate_after(
            header,
            height,
            vec![node.mempool.get(&txid).unwrap().clone()],
            77,
        );
        node.accept(carried).unwrap();

        assert!(!node.mempool.contains(&txid), "confirmed by the new branch");
    }

    #[test]
    fn the_cost_of_a_switch_is_its_depth_and_not_the_height_of_the_chain() {
        let mut shallow = a_node();
        let mut deep = a_node();
        for block in deep.branch(deep.chain.tip(), 20, 9) {
            deep.accept(block).unwrap();
        }

        let cost_of = |node: &mut Node| {
            let root = node.chain.tip();
            let losing = node.branch(root, 1, 1);
            let winning = node.branch(root, 2, 2);
            node.accept(losing[0].clone()).unwrap();
            node.accept(winning[0].clone()).unwrap();

            match node.accept(winning[1].clone()).unwrap() {
                Accepted::Reorganised {
                    undone, applied, ..
                } => (undone, applied),
                other => panic!("expected a switch, got {other:?}"),
            }
        };

        // Both halves of the work: what came off and what went on. Counting
        // only the disconnects would miss a forward walk that started at
        // genesis rather than at the fork.
        assert_eq!(cost_of(&mut shallow), (1, 2));
        assert_eq!(
            cost_of(&mut deep),
            (1, 2),
            "twenty blocks of history cost the same as none"
        );
    }

    #[test]
    fn a_block_that_failed_is_not_validated_again() {
        let mut node = a_node();
        let root = node.chain.tip();
        let mut broken = node.branch(root, 1, 1).remove(0);
        broken.transactions[0].outputs[0].value =
            Amount::from_atoms(subsidy(1).atoms() + 1).unwrap();
        assert!(broken.mine().unwrap());
        let hash = broken.header().unwrap().hash();

        assert!(node.accept(broken.clone()).is_err());
        assert!(node.chain.failed.contains(&hash));

        // Offered again as a direct extension of the same tip: the path that
        // does not consult the record is the one a peer would use.
        assert_eq!(node.accept(broken).unwrap(), Accepted::Held(hash));
        assert!(
            node.chain.body(&hash).is_none(),
            "and its body is not kept: a peer must not be able to fill memory \
             with blocks we refused"
        );
    }

    #[test]
    fn a_branch_through_a_block_that_failed_is_not_walked_again() {
        let mut node = a_node();
        let root = node.chain.tip();
        for block in node.branch(root, 2, 1) {
            node.accept(block).unwrap();
        }
        let tip = node.chain.tip();

        let mut broken = node.branch(root, 1, 2).remove(0);
        broken.transactions[0].outputs[0].value =
            Amount::from_atoms(subsidy(1).atoms() + 1).unwrap();
        assert!(broken.mine().unwrap());
        let ruined = broken.header().unwrap().hash();

        // Blocks on top of the bad one, offered in order. The third makes the
        // branch heavier, so the switch is attempted and fails on the first —
        // and every block after it would drag the node through that again.
        let mut branch = vec![broken.clone()];
        let (mut header, mut height) = (broken.header().unwrap(), 1);
        for step in 0..4u64 {
            let next = node.candidate_after(header, height, Vec::new(), 900 + step);
            header = next.header().unwrap();
            height += 1;
            branch.push(next);
        }

        for block in &branch {
            let _ = node.accept(block.clone());
        }

        assert!(node.chain.failed.contains(&ruined));
        assert_eq!(node.chain.tip(), tip, "still where it was");
        assert!(
            node.chain.body(&ruined).is_none(),
            "and nothing keeps the block that ruined it"
        );
    }

    /// The restored coin has to be judged against the tip it comes back to,
    /// not the one it was spent under — which is why the undo record carries
    /// a height and a coinbase flag at all (ADR-0012).
    #[test]
    fn a_switch_restores_a_coinbase_as_immature_against_the_new_tip() {
        let mut node = a_node();
        let root = node.chain.tip();
        let first = node.branch(root, 1, 1).remove(0);
        let mined = node.connect(first).unwrap();
        let reward = Outpoint {
            txid: node.chain.body(&mined).unwrap().transactions[0].get_tx_id(),
            v_out: 0,
        };
        assert!(node.utxo.get(&reward).unwrap().from_coinbase);

        let rival = node.branch(root, 2, 2);
        for block in rival {
            node.accept(block).unwrap();
        }

        assert!(
            node.utxo.get(&reward).is_none(),
            "the branch that paid it is gone, and so is the coin"
        );
    }

    #[test]
    fn a_block_that_arrives_before_its_parent_waits_for_it() {
        let mut node = a_node();
        let root = node.chain.tip();
        let pair = node.branch(root, 2, 1);

        let waiting = node.accept(pair[1].clone()).unwrap();

        assert_eq!(
            waiting,
            Accepted::Orphaned(pair[1].header().unwrap().hash())
        );
        assert_eq!(node.chain.height(), 0, "nothing connected yet");

        node.accept(pair[0].clone()).unwrap();

        assert_eq!(
            node.chain.tip(),
            pair[1].header().unwrap().hash(),
            "the parent arriving brings the orphan with it"
        );
    }

    #[test]
    fn a_branch_that_arrives_backwards_is_adopted_in_one_go() {
        let mut node = a_node();
        let root = node.chain.tip();
        let branch = node.branch(root, 4, 1);

        for block in branch.iter().skip(1).rev() {
            node.accept(block.clone()).unwrap();
        }
        assert_eq!(node.chain.height(), 0);

        node.accept(branch[0].clone()).unwrap();

        assert_eq!(node.chain.height(), 4);
    }

    #[test]
    fn the_orphan_pool_refuses_more_than_it_will_hold() {
        let mut node = a_node();
        let root = node.chain.tip();
        let branch = node.branch(root, MAX_ORPHANS as u32 + 2, 1);

        for block in branch.iter().skip(1) {
            node.accept(block.clone()).unwrap();
        }

        assert_eq!(node.chain.orphans.len(), MAX_ORPHANS);
    }

    #[test]
    fn an_orphan_whose_parent_never_comes_does_not_stop_the_chain() {
        let mut node = a_node();
        let root = node.chain.tip();
        let stranded = node.branch(root, 2, 1).remove(1);
        node.accept(stranded).unwrap();

        for block in node.branch(root, 3, 2) {
            node.accept(block).unwrap();
        }

        assert_eq!(node.chain.height(), 3);
    }

    #[test]
    fn an_orphan_is_something_the_node_already_holds() {
        let mut node = a_node();
        let root = node.chain.tip();
        let pair = node.branch(root, 2, 1);
        let hash = pair[1].header().unwrap().hash();

        node.accept(pair[1].clone()).unwrap();

        assert!(node.chain.holds(&hash), "asking for it again is wasted");
    }

    #[test]
    fn a_block_offered_twice_is_held_the_second_time() {
        let mut node = a_node();
        let block = node.candidate(Vec::new());

        node.accept(block.clone()).unwrap();
        let again = node.accept(block.clone()).unwrap();

        assert_eq!(again, Accepted::Held(block.header().unwrap().hash()));
        assert_eq!(node.chain.height(), 1);
    }

    #[test]
    fn genesis_is_not_a_block_to_disconnect() {
        let mut node = a_node();

        assert!(node.disconnect().is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::MAINNET;

    const TARGET_BLOCK_TIME: u32 = MAINNET.target_block_time;
    const EASY: u32 = 0x1d00ffff;
    const HARDER: u32 = 0x1c00ffff;

    fn header(parent: BlockHash, n_bits: u32, time: u32, nonce: u32) -> Header {
        Header {
            version: 1,
            previous_block_hash: parent,
            merkle_root: [0; 32],
            time,
            n_bits,
            nonce,
        }
    }

    fn genesis() -> Header {
        header(BlockHash::from_bytes([0; 32]), EASY, 1_000_000, 0)
    }

    /// `count` headers of `n_bits`, each a child of the last, starting from
    /// `from`. The nonce only makes each hash distinct; nothing here mines.
    fn extend(
        index: &mut BlockIndex,
        from: BlockHash,
        n_bits: u32,
        count: u32,
        seed: u32,
    ) -> BlockHash {
        let mut at = from;
        for step in 0..count {
            let time = index.get(&at).unwrap().header.time + TARGET_BLOCK_TIME;
            let next = header(at, n_bits, time, seed * 1_000 + step);
            at = index.insert(next).unwrap();
        }

        at
    }

    fn an_index() -> BlockIndex {
        BlockIndex::new(genesis()).unwrap()
    }

    #[test]
    fn genesis_is_at_height_zero_with_its_own_work_and_no_parent() {
        let index = an_index();
        let entry = index.best();

        assert_eq!(entry.height, 0);
        assert_eq!(entry.parent, None);
        assert_eq!(entry.total_work, genesis().work().unwrap());
    }

    #[test]
    fn a_child_takes_its_parents_height_plus_one_and_work_plus_its_own() {
        let mut index = an_index();
        let root = index.best_hash();
        let parent_work = index.best().total_work;

        let child = extend(&mut index, root, EASY, 1, 1);
        let entry = index.get(&child).unwrap();

        assert_eq!(entry.height, 1);
        assert_eq!(entry.total_work, parent_work + entry.header.work().unwrap());
    }

    #[test]
    fn a_header_whose_parent_is_unknown_is_refused_rather_than_rooted() {
        let mut index = an_index();
        let orphan = header(BlockHash::from_bytes([9; 32]), EASY, 1_000_030, 1);

        assert!(index.insert(orphan).is_err());
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn a_header_at_a_harder_target_contributes_more_work() {
        let easy = header(BlockHash::from_bytes([0; 32]), EASY, 0, 0);
        let hard = header(BlockHash::from_bytes([0; 32]), HARDER, 0, 0);

        assert!(hard.work().unwrap() > easy.work().unwrap());
    }

    #[test]
    fn the_shorter_branch_wins_when_it_carries_more_work() {
        let mut index = an_index();
        let root = index.best_hash();

        let long = extend(&mut index, root, EASY, 6, 1);
        assert_eq!(index.best_hash(), long, "six easy blocks lead for now");

        let short = extend(&mut index, root, HARDER, 2, 2);

        assert_eq!(
            index.best_hash(),
            short,
            "two hard blocks outweigh six easy ones; height would say otherwise"
        );
        assert!(index.get(&short).unwrap().height < index.get(&long).unwrap().height);
    }

    #[test]
    fn an_equal_branch_does_not_displace_the_tip_already_held() {
        let mut index = an_index();
        let root = index.best_hash();
        let first = extend(&mut index, root, EASY, 3, 1);

        extend(&mut index, root, EASY, 3, 2);

        assert_eq!(index.best_hash(), first);
    }

    #[test]
    fn both_branches_are_kept_as_tips() {
        let mut index = an_index();
        let root = index.best_hash();
        let left = extend(&mut index, root, EASY, 2, 1);
        let right = extend(&mut index, root, EASY, 3, 2);

        let tips = index.tips();

        assert_eq!(tips.len(), 2);
        assert!(tips.contains(&left) && tips.contains(&right));
    }

    #[test]
    fn a_header_offered_twice_is_recorded_once() {
        let mut index = an_index();
        let child = header(index.best_hash(), EASY, 1_000_030, 1);

        let first = index.insert(child).unwrap();
        let again = index.insert(child).unwrap();

        assert_eq!(first, again);
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn ancestry_walks_back_from_a_block_and_returns_it_oldest_first() {
        let mut index = an_index();
        let root = index.best_hash();
        let tip = extend(&mut index, root, EASY, 5, 1);

        let walked = index.ancestry(&tip, 3);

        assert_eq!(walked.len(), 3);
        assert_eq!(walked[0].height, 3);
        assert_eq!(walked[2].height, 5);
    }

    #[test]
    fn ancestry_stops_at_genesis_rather_than_asking_for_more() {
        let mut index = an_index();
        let root = index.best_hash();
        let tip = extend(&mut index, root, EASY, 2, 1);

        assert_eq!(index.ancestry(&tip, 100).len(), 3);
    }

    #[test]
    fn the_window_a_retarget_needs_is_reachable_without_walking_twice() {
        let mut index = an_index();
        let root = index.best_hash();
        let tip = extend(&mut index, root, EASY, RETARGET_WINDOW as u32 + 5, 1);

        let bits = index.required_bits_after(&tip, &MAINNET).unwrap();

        assert_eq!(bits, EASY, "blocks arriving on time change nothing");
    }

    #[test]
    fn the_median_of_a_branch_is_taken_from_that_branch() {
        let mut index = an_index();
        let root = index.best_hash();
        let tip = extend(&mut index, root, EASY, 20, 1);

        let median = index.median_time_after(&tip).unwrap();
        let expected = index.get(&tip).unwrap().header.time - 5 * TARGET_BLOCK_TIME;

        assert_eq!(median, expected);
    }
}
