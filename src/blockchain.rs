use crate::block::{Block, BlockHash, Header};
use crate::difficulty::{median_time_past, required_bits, MEDIAN_TIME_SPAN, RETARGET_WINDOW};
use crate::mempool::Mempool;
use crate::params::Network;
use crate::transaction::Transaction;
use crate::utxo::{Undo, UtxoSet};
use crate::validation::check_block;
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
    /// Held in memory. [ADR-0013](../docs/adr/0013-persistence.md) makes both
    /// durable in M5; until then a node that dies mid-reorg cannot recover.
    bodies: HashMap<BlockHash, Block>,
    undo: HashMap<BlockHash, Vec<Undo>>,
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

    /// Validates a block against the current tip and applies it. Nothing moves
    /// until validation has passed, so a block refused leaves the set and the
    /// tip exactly as they were.
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

        check_block(&block, &self.index, utxo, now, network)?;

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

        let hash = self.index.insert(header)?;
        self.bodies.insert(hash, block);
        self.undo.insert(hash, spent);
        self.tip = hash;

        Ok(hash)
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
        self.tip = parent;

        // Everything but the coinbase goes back, and only what is still valid
        // against the set as it now stands.
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
    use crate::difficulty::TARGET_BLOCK_TIME;
    use crate::params::TESTNET;
    use crate::transaction::Outpoint;
    use crate::utxo::UtxoView;
    use crate::validation::check_spend;
    use crate::validation::fixtures::{funded, pay_to, signed};

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

    #[test]
    fn a_restored_coinbase_output_is_immature_again() {
        let mut node = a_node();
        let block = node.candidate(Vec::new());
        let hash = node.connect(block).unwrap();
        let coinbase = node.chain.body(&hash).unwrap().transactions[0].get_tx_id();

        node.disconnect().unwrap();

        assert!(
            node.utxo
                .get(&Outpoint {
                    txid: coinbase,
                    v_out: 0
                })
                .is_none(),
            "the coin the block created is gone with it"
        );
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
    use crate::difficulty::TARGET_BLOCK_TIME;
    use crate::params::MAINNET;

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
