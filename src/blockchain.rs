use crate::block::{Block, BlockHash, Header, SharedHash};
use crate::difficulty::{
    median_time_past, required_bits, too_far_ahead, MAX_FUTURE_DRIFT, MEDIAN_TIME_SPAN,
    RETARGET_WINDOW,
};
use crate::mempool::Mempool;
use crate::messages::headers::{MAX_HEADERS, MAX_LOCATOR};
use crate::params::Network;
use crate::persist::Storage;
use crate::transaction::{Outpoint, Transaction};
use crate::utxo::{Coin, Undo, UtxoSet};
use crate::validation::{check_block, check_body, check_header, ClockDrift};
use anyhow::{anyhow, bail, Context, Result};
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
    /// The best chain by height, genesis first. Answering a locator and
    /// finding what to fetch are both height lookups on this rather than walks
    /// back through parent pointers, and a peer can ask as often as it likes.
    best_chain: Vec<BlockHash>,
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
            best_chain: vec![hash],
        })
    }

    /// Rebuilds an index from what a store held, in the order `insert`
    /// requires: a header is never seen before its parent.
    ///
    /// A header whose parent is nowhere in the set is corruption rather than
    /// an orphan — nothing writes one — and is said so rather than dropped.
    pub fn restored(genesis: Header, headers: &[Header]) -> Result<BlockIndex> {
        let mut index = BlockIndex::new(genesis)?;
        let root = genesis.hash();

        let mut children: HashMap<BlockHash, Vec<(BlockHash, Header)>> = HashMap::new();
        for header in headers {
            let hash = header.hash();
            if hash != root {
                children
                    .entry(header.previous_block_hash)
                    .or_default()
                    .push((hash, *header));
            }
        }

        // A restart cannot remember which of two equal-work tips arrived
        // first, so it settles the tie by hash instead. Arbitrary, but the
        // same arbitrary answer every time — a node that came back on a
        // different branch each restart would be worse.
        for siblings in children.values_mut() {
            siblings.sort_by_key(|(hash, _)| *hash);
            siblings.dedup_by_key(|(hash, _)| *hash);
        }

        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for (hash, header) in children.remove(&parent).unwrap_or_default() {
                index.insert(header)?;
                frontier.push(hash);
            }
        }

        if let Some((_, stranded)) = children.values().flatten().next() {
            bail!(
                "{} stored headers descend from no known block, starting with {}",
                children.values().map(Vec::len).sum::<usize>(),
                stranded.hash()
            );
        }

        Ok(index)
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
        let extends_best = header.previous_block_hash == self.best;
        self.entries.insert(hash, entry);

        if outweighs {
            self.best = hash;
            if extends_best {
                self.best_chain.push(hash);
            } else {
                self.rebuild_best_chain();
            }
        }

        Ok(hash)
    }

    fn rebuild_best_chain(&mut self) {
        self.best_chain = self
            .ancestry(&self.best, usize::MAX)
            .into_iter()
            .map(|entry| entry.header.hash())
            .collect();
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

    /// Every entry no other entry claims as a parent.
    #[cfg(test)]
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

    /// Names this chain in `log(height)` hashes: ten one at a time from the
    /// tip, then doubling, always ending at genesis. A peer finds where the
    /// two agree without either sending a chain.
    pub fn locator(&self, from: &BlockHash) -> Vec<BlockHash> {
        let mut locator = Vec::new();
        let mut at = Some(*from);
        let mut step = 1;

        while let Some(hash) = at {
            let Some(entry) = self.entries.get(&hash) else {
                break;
            };
            locator.push(hash);

            if locator.len() > 10 {
                step *= 2;
            }
            at = self
                .ancestry(&hash, step + 1)
                .first()
                .map(|e| e.header.hash());
            if at == Some(hash) {
                break;
            }
            if entry.parent.is_none() {
                break;
            }
        }

        let genesis = self
            .ancestry(from, usize::MAX)
            .first()
            .map(|e| e.header.hash());
        match genesis {
            Some(root) if !locator.contains(&root) => locator.push(root),
            _ => {}
        }

        locator.truncate(MAX_LOCATOR);
        locator
    }

    /// Where `hash` sits on the best chain, if it is on it at all.
    pub fn height_on_best(&self, hash: &BlockHash) -> Option<usize> {
        let height = self.entries.get(hash)?.height as usize;

        (self.best_chain.get(height) == Some(hash)).then_some(height)
    }

    /// The headers that follow the **newest** hash in `locator` this node has
    /// on its own best chain, oldest first, at most `MAX_HEADERS` of them.
    ///
    /// Newest, not any: a locator always ends at genesis, so taking the first
    /// match would answer every request from height one and a peer past
    /// `MAX_HEADERS` would never make progress.
    ///
    /// Nothing in the locator matching means the peer is on a chain we have
    /// never heard of, and the honest answer is our own from genesis.
    pub fn headers_after(&self, locator: &[BlockHash], stop: &BlockHash) -> Vec<Header> {
        let agreed = locator
            .iter()
            .filter_map(|hash| self.height_on_best(hash))
            .max();

        self.best_chain
            .iter()
            .skip(agreed.map(|at| at + 1).unwrap_or(0))
            .take(MAX_HEADERS)
            .filter_map(|hash| self.entries.get(hash))
            .map(|entry| entry.header)
            .take_while(|header| header.hash() != *stop)
            .collect()
    }

    /// The best chain by height, genesis first.
    pub fn best_chain(&self) -> &[BlockHash] {
        &self.best_chain
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
    /// Bounded by what has *not* been applied once there is storage: the
    /// authority is then `blocks.dat` and `undo.dat`, an applied block is
    /// dropped from here, and a restart comes back with these empty.
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
    /// Bodies refused for something their block's hash does not identify, so
    /// the hash could not be recorded. Keyed on the body rather than the hash
    /// precisely because a different body may share it — which is the whole
    /// reason those refusals are exempt.
    refused_bodies: HashSet<[u8; 32]>,
    /// Absent in tests and wherever a chain is a scratch chain. Present, it is
    /// the authority, and the memory above is a cache in front of it.
    storage: Option<std::sync::Arc<Storage>>,
    /// The chain as applied, genesis first, kept in step with `tip`. Every
    /// height lookup an operator or a peer makes reads this, so recomputing it
    /// would be a walk and a hash per block, under the lock.
    connected: Vec<BlockHash>,
    /// Payments a disconnect handed back, waiting for a caller that can
    /// re-admit them without the lock. Bounded by what a reorg disconnected,
    /// and emptied by whoever takes them.
    returned: Vec<Transaction>,
}

/// A block's validation, and what it was validated against.
///
/// The coins are the ones the set held when the check started. `apply` cannot
/// trust a verdict about coins that have since moved, so this is what makes
/// the answer re-checkable rather than merely old.
#[derive(Debug)]
pub struct Checking {
    on: BlockHash,
    height: u32,
    coins: HashMap<Outpoint, Coin>,
}

impl Checking {
    /// Runs the expensive half — a signature and a script per input — and
    /// returns the only thing `accept_vouched` will take.
    ///
    /// The caller is expected to do this **without the node lock**. Nothing
    /// here can enforce that. What is enforced is the other half: `Vouched`
    /// has no public constructor, so a block cannot reach `accept_vouched`
    /// without having been through `check_body`. Left to a comment, the next
    /// caller — a second relay path, a resync, a refactor that reorders three
    /// steps — would be one mistake away from applying a block nothing
    /// validated, with only `UtxoSet::connect` between it and the tip.
    pub fn vouch(self, block: &Block, network: Network) -> Result<Vouched> {
        check_body(block, self.height, &self.coins, network)?;

        Ok(Vouched(self))
    }

    /// The coins the block's inputs name, as the set held them. What is
    /// *missing* is as much of the answer as what is here: an input naming
    /// something an earlier transaction in the same block creates finds
    /// nothing, and the `UtxoView` layered over this supplies it.
    #[cfg(test)]
    pub fn coins(&self) -> &HashMap<Outpoint, Coin> {
        &self.coins
    }
}

/// A block that has passed `check_body`, and what it passed against.
///
/// The field is private and `Checking::vouch` is the only way to make one, so
/// "this was validated" is a fact the type carries rather than a discipline a
/// caller keeps. That matters more here than anywhere else in the program:
/// `accept_vouched` skips validation by design, and the only other thing
/// between a bad block and the tip is `UtxoSet::connect` — which knows about
/// double-spends and nothing about signatures, amounts or the coinbase claim.
#[derive(Debug)]
pub struct Vouched(Checking);

/// How many blocks may wait for a parent. Filling this costs real work: a
/// block is only held once its header has been shown to meet its own target.
pub const MAX_ORPHANS: usize = 64;

/// How many refused bodies to remember. Each is 32 bytes, and remembering one
/// saves recomputing a merkle root over a megabyte of transactions.
pub const MAX_REFUSED_BODIES: usize = 4_096;

fn body_digest(block: &Block) -> [u8; 32] {
    block
        .get_raw_format()
        .map(|raw| crate::util::get_hash(&raw))
        .unwrap_or([0; 32])
}

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
            connected: vec![tip],
            returned: Vec::new(),
            bodies: HashMap::from([(tip, genesis.clone())]),
            undo: HashMap::from([(tip, Vec::new())]),
            failed: HashSet::new(),
            orphans: HashMap::new(),
            refused_bodies: HashSet::new(),
            storage: None,
        })
    }

    /// A chain that was already there, and the set that goes with it.
    ///
    /// Genesis is written on the first open, so a store that holds no headers
    /// is a new node rather than a corrupt one. After that the index comes
    /// from the store and the tip from the marker — which is ordinarily
    /// *behind* the index's best, because headers arrive ahead of bodies.
    /// `catch_up` closes that gap, and it is the same path a running node
    /// takes when a body finally arrives.
    pub fn open(genesis: &Block, storage: Storage) -> Result<(Chain, UtxoSet)> {
        let header = genesis.header()?;
        let restored = storage.restored()?;
        if restored.headers.is_empty() {
            let mut fresh = Chain::new(genesis)?;
            let mut utxo = UtxoSet::new();
            for transaction in &genesis.transactions {
                utxo.connect(transaction, 0)
                    .context("seeding the UTXO set from the genesis block")?;
            }
            storage.begin(&header, genesis, &utxo)?;
            fresh.storage = Some(std::sync::Arc::new(storage));
            return Ok((fresh, utxo));
        }

        let index = BlockIndex::restored(header, &restored.headers)?;
        // A store with headers but no marker cannot say where its coins are,
        // and starting at genesis on a set that is not genesis's would fail
        // later, naming the wrong thing.
        let tip = restored
            .best_block
            .context("the store holds an index but no best-block marker")?;
        if !index.contains(&tip) {
            bail!("the best-block marker names {tip}, which the index does not hold");
        }

        let connected = index
            .ancestry(&tip, usize::MAX)
            .into_iter()
            .map(|entry| entry.header.hash())
            .collect();

        Ok((
            Chain {
                index,
                tip,
                connected,
                returned: Vec::new(),
                bodies: HashMap::new(),
                undo: HashMap::new(),
                failed: HashSet::new(),
                orphans: HashMap::new(),
                refused_bodies: HashSet::new(),
                storage: Some(std::sync::Arc::new(storage)),
            },
            restored.utxo,
        ))
    }

    /// Connects forward from the marker to the best branch the index knows,
    /// as far as the bodies on disk allow. Stops rather than fails: a body the
    /// node never received is a thing to ask a peer for, not a corruption.
    pub fn catch_up(
        &mut self,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        now: u32,
        network: Network,
    ) -> Result<usize> {
        let mut applied = 0;

        while self.tip != self.index.best_hash() {
            let Some(target) = self.furthest_we_hold()? else {
                break;
            };
            match self.switch_to(target, utxo, mempool, now, network) {
                Ok((_, forward)) => applied += forward,
                // A block on disk that no longer validates is a branch to
                // abandon, not a reason a node cannot start. `switch_to` puts
                // the chain back where it was, and `failed` stops it being
                // tried again.
                Err(_) => break,
            }
        }

        Ok(applied)
    }

    /// The furthest block along the best chain we could actually connect to:
    /// every body from genesis up to it is on disk, and it is heavier than
    /// where we are. A body we never received is a thing to ask a peer for
    /// rather than a corruption, so catching up stops there instead of
    /// failing — and `None` is what ends the loop.
    fn furthest_we_hold(&self) -> Result<Option<BlockHash>> {
        let mut furthest = None;
        for hash in self.index.best_chain() {
            // Whether the block is there, not what it says. Reading it to find
            // out would parse the whole chain off disk to answer a question
            // the offsets already answer.
            if !self.holds(hash) {
                break;
            }
            furthest = Some(*hash);
        }

        let Some(candidate) = furthest else {
            return Ok(None);
        };

        Ok((self.work_of(&candidate)? > self.work_of(&self.tip)?).then_some(candidate))
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

    /// Whatever the node holds without touching a disk. `getdata` uses this
    /// under the lock, then reads the rest through `files` with the lock let
    /// go — a peer names up to `MAX_INVENTORY` blocks, and holding the node
    /// still across that many seeks is the stall `record` is shaped to avoid.
    pub fn cached_body(&self, hash: &BlockHash) -> Option<Block> {
        self.bodies.get(hash).cloned()
    }

    /// The files, to read from without the node lock. An `Arc` rather than a
    /// borrow for exactly that reason.
    pub fn files(&self) -> Option<std::sync::Arc<Storage>> {
        self.storage.clone()
    }

    /// The chain the node has actually **connected**, genesis first.
    ///
    /// Not `BlockIndex::best_chain`, which is the heaviest headers: while a
    /// node is behind — or has a fork of its own — the two are different
    /// chains, not a prefix of one another, and describing a block "at height
    /// 2" from the header chain would name one this node does not have.
    ///
    /// Kept, not recomputed. Walking the ancestry and hashing each header is a
    /// double-SHA256 per block of chain, and the callers hold the node lock
    /// while they ask.
    pub fn connected(&self) -> &[BlockHash] {
        &self.connected
    }

    /// Whether a block is on the chain this node has connected, and where.
    pub fn height_of(&self, hash: &BlockHash) -> Option<usize> {
        let height = self.index.get(hash)?.height as usize;

        (self.connected.get(height) == Some(hash)).then_some(height)
    }

    /// From memory, or from `blocks.dat`. A restart leaves the maps empty and
    /// the files full, which is what lets a restarted node serve a block to a
    /// peer and undo one in a reorg.
    ///
    /// A disk read is **not** kept: `getdata` reaches this, so caching what it
    /// returns would let a peer walking the chain fill our memory with blocks
    /// we already have on disk.
    pub fn body(&self, hash: &BlockHash) -> Option<Block> {
        if let Some(block) = self.bodies.get(hash) {
            return Some(block.clone());
        }

        self.storage.as_ref()?.block(hash).ok().flatten()
    }

    fn undo_for(&self, hash: &BlockHash) -> Option<Vec<Undo>> {
        if let Some(spent) = self.undo.get(hash) {
            return Some(spent.clone());
        }

        self.storage.as_ref()?.undo(hash).ok().flatten()
    }

    /// Whether this is a block we already have, connected or waiting — which
    /// is the question an `inv` asks.
    pub fn holds(&self, hash: &BlockHash) -> bool {
        self.bodies.contains_key(hash)
            || self.orphans.contains_key(hash)
            || self
                .storage
                .as_ref()
                .is_some_and(|storage| storage.knows(hash))
    }

    /// Takes a block from anywhere and decides what it means: an extension of
    /// the tip, a heavier branch worth moving to, or something recorded and
    /// left alone. Chain selection is by cumulative work, never by height.
    /// What a block needs checking against, taken under the lock and worked on
    /// without it. Nothing here borrows the node.
    ///
    /// `Vouched` is what comes back: the block, and the coins it was checked
    /// against, so `accept_vouched` can confirm none of them moved.
    pub fn to_check(
        &self,
        block: &Block,
        utxo: &UtxoSet,
        now: u32,
        network: Network,
    ) -> Option<Checking> {
        let header = block.header().ok()?;
        // Only a block extending the tip. A reorg re-validates a whole branch
        // against a set that moves under it; that is the shape ADR-0020 says
        // the optimistic path does not transfer to, and it stays under the
        // lock. This is the case a stranger can drive at will.
        if header.previous_block_hash != self.tip {
            return None;
        }
        if self.undo.contains_key(&header.hash()) || self.failed.contains(&header.hash()) {
            return None;
        }
        // The same question `apply` asks. A `ClockDrift` refusal remembers the
        // body rather than the hash, and time only ever makes that refusal
        // stale — so without this the two paths would give different answers
        // about the same block, and a reorg through it would refuse what a
        // relay of it accepted.
        if self.refused_bodies.contains(&body_digest(block)) {
            return None;
        }

        // The header's own checks need the index, which is behind this lock,
        // and are cheap — so they happen here rather than being copied out.
        // A refusal is left to the ordinary path, which is the one place that
        // decides what a refusal *means*.
        let height = check_header(block, &self.index, now, network).ok()?;

        Some(Checking {
            on: self.tip,
            height,
            coins: utxo.coins_for_block(&block.transactions),
        })
    }

    /// Applies a block whose signatures were checked without the lock.
    ///
    /// The re-check is two questions, and both have to be yes: is the tip
    /// still the one it was checked against, and is every coin it was checked
    /// against still there and unchanged? A no to either means the answer was
    /// about a chain that no longer exists, so it is thrown away and the block
    /// goes through the ordinary path — which validates under the lock.
    pub fn accept_vouched(
        &mut self,
        block: Block,
        vouched: &Vouched,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
        now: u32,
        network: Network,
    ) -> Result<Accepted> {
        let checking = &vouched.0;
        if self.tip != checking.on
            || !checking
                .coins
                .iter()
                .all(|(outpoint, coin)| utxo.get(outpoint).as_ref() == Some(coin))
        {
            return self.accept(block, utxo, mempool, now, network);
        }

        let header = block.header()?;
        let hash = header.hash();
        self.index.insert(header)?;
        self.remember(&[header])?;
        self.bodies.insert(hash, block);

        self.apply_vouched(hash, utxo, mempool)?;
        self.adopt_orphans(utxo, mempool, now, network);

        Ok(Accepted::Extended(hash))
    }

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

        // The cheapest check that costs a stranger something. Everything below
        // this either stores the block or walks the chain for it, and a header
        // nobody mined should reach neither.
        if !header.meets_its_target()? {
            bail!("{hash} does not meet its own target");
        }

        if !self.index.contains(&header.previous_block_hash) {
            return Ok(if self.orphan(hash, block) {
                Accepted::Orphaned(hash)
            } else {
                // The pool is full. Saying so rather than `Orphaned` is what
                // stops the caller asking for a parent we did not keep.
                Accepted::Held(hash)
            });
        }

        self.index.insert(header)?;
        self.remember(&[header])?;
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

    /// Whether it was kept. The newcomer is dropped when the pool is full
    /// rather than evicting an older one, because the older ones are the ones
    /// whose parents may still be on the way.
    /// Records a header without its body: enough to know the branch is real
    /// and how much work it carries, which is what decides whether the body is
    /// worth asking for at all.
    ///
    /// Everything a header can be judged on alone is checked here — the work
    /// it claims, the target the rule requires, and its timestamp — so a
    /// stranger's bulk data is never fetched before its work has been.
    #[cfg(test)]
    pub fn add_header(&mut self, header: Header, now: u32, network: Network) -> Result<BlockHash> {
        let hash = self.take_header(header, now, network)?;
        self.remember(&[header])?;

        Ok(hash)
    }

    /// Every header in a peer's batch that the index took, in **one** commit.
    /// The caller holds the node lock while this runs, so a batch of two
    /// thousand being two thousand durable writes is the difference between a
    /// stalled node and a working one.
    ///
    /// Returns how many were new. Nothing here carries a block offset or moves
    /// the marker, which is why the marker ordinarily sits behind the index's
    /// best tip.
    pub fn add_headers(
        &mut self,
        headers: &[Header],
        now: u32,
        network: Network,
    ) -> (usize, Option<String>) {
        // One at a time and in order, because a header's parent may be the one
        // before it in the same batch.
        let mut taken = Vec::with_capacity(headers.len());
        let mut refused = None;

        for header in headers {
            match self.take_header(*header, now, network) {
                Ok(_) => taken.push(*header),
                // The first reason, not every reason: a peer sending two
                // thousand bad headers has one problem, and the log is
                // bounded. The caller has a logging seam and this does not.
                Err(why) => refused = refused.or(Some(format!("{why:#}"))),
            }
        }

        // A header the store did not take is one the index holds and disk does
        // not; the next start asks a peer for it again. That is a better cost
        // than failing a running node over a write.
        let _ = self.remember(&taken);

        (taken.len(), refused)
    }

    fn take_header(&mut self, header: Header, now: u32, network: Network) -> Result<BlockHash> {
        let hash = header.hash();
        if self.index.contains(&hash) {
            return Ok(hash);
        }

        let parent = header.previous_block_hash;
        if !self.index.contains(&parent) {
            bail!("{hash} follows {parent}, which this node does not know");
        }

        if !header.meets_its_target()? {
            bail!("{hash} does not meet its own target");
        }

        let required = self.index.required_bits_after(&parent, network)?;
        if header.n_bits != required {
            bail!(
                "{hash} states n_bits {:#010x} where the rule requires {required:#010x}",
                header.n_bits
            );
        }

        if too_far_ahead(header.time, now) {
            bail!(
                "{hash} claims a time more than {MAX_FUTURE_DRIFT}s ahead of this node's \
                 clock; check this machine's time before suspecting the network"
            );
        }

        let median = self.index.median_time_after(&parent)?;
        if header.time <= median {
            bail!("{hash} is not past the median of the last eleven");
        }

        self.index.insert(header)
    }

    fn remember(&mut self, headers: &[Header]) -> Result<()> {
        match &self.storage {
            Some(storage) => storage.remember_headers(headers),
            None => Ok(()),
        }
    }

    /// Blocks on the best chain whose bodies this node does not have, oldest
    /// first — what headers-first sync asks for once the headers check out.
    pub fn bodies_wanted(&self, at_most: usize) -> Vec<BlockHash> {
        self.index
            .best_chain()
            .iter()
            .filter(|hash| !self.holds(hash) && !self.failed.contains(hash))
            .take(at_most)
            .copied()
            .collect()
    }

    /// How many blocks on the best chain this node knows of but has not got.
    /// The honest answer to "how far behind am I".
    pub fn bodies_missing(&self) -> usize {
        self.bodies_wanted(usize::MAX).len()
    }

    pub fn locator(&self) -> Vec<BlockHash> {
        self.index.locator(&self.tip)
    }

    pub fn headers_after(&self, locator: &[BlockHash], stop: &BlockHash) -> Vec<Header> {
        self.index.headers_after(locator, stop)
    }

    fn remember_refused_body(&mut self, block: &Block) {
        if self.refused_bodies.len() < MAX_REFUSED_BODIES {
            self.refused_bodies.insert(body_digest(block));
        }
    }

    fn orphan(&mut self, hash: BlockHash, block: Block) -> bool {
        if self.orphans.len() >= MAX_ORPHANS && !self.orphans.contains_key(&hash) {
            return false;
        }

        self.orphans.insert(hash, block);
        true
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
    #[cfg(test)]
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

    /// The half of `apply` after validation, for a block whose signatures were
    /// checked without the lock and whose coins have been confirmed unchanged.
    /// Everything it does is what `apply` does once `check_block` has passed.
    fn apply_vouched(
        &mut self,
        hash: BlockHash,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
    ) -> Result<()> {
        let block = self
            .body(&hash)
            .ok_or_else(|| anyhow!("{hash} has no body to apply"))?;

        self.connect_body(hash, &block, utxo, mempool)
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
            .body(&hash)
            .ok_or_else(|| anyhow!("{hash} has no body to apply"))?;

        if self.refused_bodies.contains(&body_digest(&block)) {
            bail!("{hash} carries a body this node has already refused");
        }

        if let Err(refusal) = check_block(&block, &self.index, utxo, now, network) {
            // Two refusals must not be recorded against the hash: `ClockDrift`
            // is about time rather than the block, and `SharedHash` says which
            // body the hash does not identify. Both types carry the reason.
            let keep_the_hash_clean = refusal.downcast_ref::<ClockDrift>().is_some()
                || refusal.downcast_ref::<SharedHash>().is_some();

            if keep_the_hash_clean {
                // The body, not the hash. Identical bytes are refused without
                // being revalidated; a different body sharing the hash is not.
                self.remember_refused_body(&block);
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

        self.connect_body(hash, &block, utxo, mempool)
    }

    /// Everything after a block has passed validation: the set, the files, the
    /// store, the mempool and the tip. Shared by the path that validates under
    /// the lock and the one that validates without it, so the two cannot come
    /// to apply a block differently.
    fn connect_body(
        &mut self,
        hash: BlockHash,
        block: &Block,
        utxo: &mut UtxoSet,
        mempool: &mut Mempool,
    ) -> Result<()> {
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

        // The block's bytes and its undo record reach the files, flushed,
        // before the commit that records them — and the commit carries the
        // coins and the marker with it, so a crash lands on one side or the
        // other. A failure here unwinds, because a set that moved without the
        // store is the one state nothing can recover from.
        if let Some(storage) = &self.storage {
            if let Err(broken) = storage.remember_block(&block.header()?, block, &spent, height) {
                unwind(utxo, &block.transactions, &spent);
                return Err(broken.context("recording a block that validated"));
            }
        }

        for transaction in &block.transactions {
            mempool.remove(&transaction.get_tx_id());
        }

        // Held in memory only while there is nowhere else to hold them. With
        // storage, the block and its undo record are durable by now, and
        // keeping them would make a long-running node's footprint grow with
        // its chain for nothing.
        if self.storage.is_some() {
            self.bodies.remove(&hash);
        } else {
            self.undo.insert(hash, spent);
        }
        self.tip = hash;
        self.connected.push(hash);

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

        let tip = self.tip;
        let block = self
            .body(&tip)
            .ok_or_else(|| anyhow!("{tip} has no body to undo"))?;
        let spent = self
            .undo_for(&tip)
            .ok_or_else(|| anyhow!("{tip} has no undo record"))?;

        // The commit first, and the set after: a disconnect writes no files,
        // so a failed commit has to leave nothing moved, and `unwind` cannot
        // fail once it starts. A crash between the two costs the memory, which
        // a restart rebuilds from the store anyway.
        if let Some(storage) = &self.storage {
            storage
                .remember_disconnect(&parent, &block, &spent)
                .context("recording a disconnect")?;
        }

        unwind(utxo, &block.transactions, &spent);

        self.undo.remove(&self.tip);
        self.tip = parent;
        self.connected.pop();

        // Everything but the coinbase is *offered back* — handed to the
        // caller rather than validated here. Re-admitting them means a
        // signature and a script per input, and doing that under the lock is
        // the thing #115 exists to stop: a stranger's reorg would hold the
        // node still for a whole block's worth of curve arithmetic.
        //
        // A refusal is the ordinary case — a payment that depended on the
        // block is not one to keep — and dropping it costs nothing: the
        // mempool is not consensus, and whoever relayed it still has it.
        self.returned.extend(block.transactions.into_iter().skip(1));
        let _ = mempool;
        let _ = network;

        Ok(parent)
    }

    /// Transactions a disconnect handed back, for the caller to re-admit
    /// **without the lock**. Taken, not read: they are offered once.
    pub fn returned(&mut self) -> Vec<Transaction> {
        std::mem::take(&mut self.returned)
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
    use crate::block::merkle_root;
    use crate::crypto::PrivateKey;
    use crate::params::TESTNET;
    use crate::transaction::Outpoint;
    use crate::utxo::UtxoView;
    use crate::validation::check_spend;
    use crate::validation::fixtures::{funded, pay_to, signed};
    use rstest::rstest;

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

        /// Disconnects and re-admits, which is the pair the protocol layer
        /// performs — the second half without the node lock. A test that did
        /// only the first would be testing half a caller.
        fn disconnect(&mut self) -> Result<BlockHash> {
            let parent = self
                .chain
                .disconnect(&mut self.utxo, &mut self.mempool, &TESTNET)?;
            self.readmit();

            Ok(parent)
        }

        fn readmit(&mut self) {
            let height = self.chain.height() + 1;
            for transaction in self.chain.returned() {
                let _ = self
                    .mempool
                    .accept(transaction, &self.utxo, height, &TESTNET);
            }
        }

        fn coins(&self) -> Vec<(Outpoint, crate::utxo::Coin)> {
            let mut coins = self.utxo.coins();
            coins.sort_by_key(|(outpoint, _)| (outpoint.txid.to_string(), outpoint.v_out));
            coins
        }
    }

    /// `connected` is kept in step with `tip` rather than recomputed, so
    /// every path that moves the tip has to move it too. A drift here would
    /// make `/block/height/{n}` name a block from a chain the node is not on.
    #[test]
    fn the_connected_chain_is_the_ancestry_of_the_tip_after_every_move() {
        let mut node = a_node();
        let root = node.chain.tip();

        let assert_in_step = |chain: &Chain| {
            let walked: Vec<BlockHash> = chain
                .index()
                .ancestry(&chain.tip(), usize::MAX)
                .into_iter()
                .map(|entry| entry.header.hash())
                .collect();
            assert_eq!(chain.connected(), walked, "the kept chain drifted");
        };

        assert_in_step(&node.chain);
        for _ in 0..3 {
            let next = node.candidate(Vec::new());
            node.connect(next).unwrap();
            assert_in_step(&node.chain);
        }

        node.chain
            .disconnect(&mut node.utxo, &mut node.mempool, &TESTNET)
            .unwrap();
        assert_in_step(&node.chain);

        // And across a reorg, which retreats and then applies forward.
        let heavier = node.branch(root, 5, 11);
        for block in heavier {
            node.accept(block).unwrap();
        }
        assert_in_step(&node.chain);
        assert_eq!(
            node.chain.connected().len(),
            node.chain.height() as usize + 1
        );
    }

    /// The two questions the re-check asks, and what each protects.
    ///
    /// A verdict reached without the lock is a verdict about a chain that may
    /// have moved since. If the tip moved, the block is no longer an extension
    /// of anything this node has; if a coin moved, the signatures were checked
    /// against something that is not there. Either way the answer is thrown
    /// away and the block goes back through the path that validates under the
    /// lock — which is the one that can still accept it, or refuse it
    /// properly.
    #[test]
    fn a_verdict_about_a_tip_that_has_since_moved_is_not_used() {
        let mut node = a_node();
        let first = node.candidate(Vec::new());
        let vouched = node
            .chain
            .to_check(&first, &node.utxo, node.now, &TESTNET)
            .expect("an extension of the tip")
            .vouch(&first, &TESTNET)
            .expect("a block this node would accept");

        // Somebody else's block lands first, so the tip is not the one this
        // was checked against.
        let jumped = node.candidate(Vec::new());
        node.connect(jumped).unwrap();

        let outcome = node.chain.accept_vouched(
            first,
            &vouched,
            &mut node.utxo,
            &mut node.mempool,
            node.now,
            &TESTNET,
        );

        // Held, not applied: the ordinary path took it and found it does not
        // build on the tip.
        assert!(matches!(outcome.unwrap(), Accepted::Held(_)));
    }

    /// The coins half of the same question. Today the tip check covers it —
    /// only a connect or a disconnect moves the set, and both move the tip —
    /// but that is an invariant held by convention rather than by a type, and
    /// this is the check that does not depend on it holding.
    ///
    /// What distinguishes the two paths is not *that* the block is refused —
    /// `UtxoSet::connect` refuses it either way — but that the refusal is
    /// **recorded**. The ordinary path marks the hash failed, so the same
    /// block offered again costs a lookup; applying on a stale verdict and
    /// failing in `connect_body` records nothing, and a peer could offer it
    /// for ever.
    #[test]
    fn a_verdict_about_a_coin_that_has_since_moved_goes_back_through_the_ordinary_path() {
        let mut node = a_node();
        let key = PrivateKey::random();
        let outpoint = funded(&mut node.utxo, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let block = node.candidate(vec![payment.clone()]);

        let checking = node
            .chain
            .to_check(&block, &node.utxo, node.now, &TESTNET)
            .expect("an extension of the tip");
        assert!(checking.coins().contains_key(&outpoint));
        let vouched = checking.vouch(&block, &TESTNET).unwrap();

        // The coin goes out from under the verdict, without the tip moving.
        node.utxo.connect(&payment, 1).unwrap();

        let outcome = node.chain.accept_vouched(
            block.clone(),
            &vouched,
            &mut node.utxo,
            &mut node.mempool,
            node.now,
            &TESTNET,
        );

        assert!(outcome.is_err(), "it must not be applied on an old verdict");
        let hash = block.header().unwrap().hash();
        assert_eq!(
            node.chain
                .accept(block, &mut node.utxo, &mut node.mempool, node.now, &TESTNET)
                .unwrap(),
            Accepted::Held(hash),
            "and the refusal was remembered, so offering it again costs a lookup"
        );
    }

    /// The header's own checks run in `to_check`, under the lock, because
    /// they need the index. Nothing else pins that they run at all — every
    /// other "an invalid block is refused" test drives `accept` directly, and
    /// this is the door every relayed tip extension now takes.
    #[rstest]
    #[case::wrong_bits(|block: &mut Block| block.n_bits = 0x1d00ffff)]
    #[case::not_past_the_median(|block: &mut Block| block.time = 0)]
    #[case::from_the_future(|block: &mut Block| block.time = u32::MAX)]
    fn a_header_that_does_not_check_out_never_reaches_the_path_without_the_lock(
        #[case] spoil: fn(&mut Block),
    ) {
        let node = a_node();
        let mut block = node.candidate(Vec::new());
        spoil(&mut block);

        assert!(
            node.chain
                .to_check(&block, &node.utxo, node.now, &TESTNET)
                .is_none(),
            "a header the index refuses is not one to check without the lock"
        );
    }

    /// A body already refused is refused by both paths, or the node follows
    /// one chain by relay and another by reorg.
    #[test]
    fn a_body_this_node_has_already_refused_is_not_checked_again_without_the_lock() {
        let mut node = a_node();
        let mut early = node.candidate(Vec::new());
        // A `ClockDrift` refusal remembers the *body*, and time only ever
        // makes that refusal stale.
        early.time = node.now + crate::difficulty::MAX_FUTURE_DRIFT + 60;
        early.nonce = early.search(0, u32::MAX).unwrap();
        early.seal().unwrap();

        assert!(node
            .chain
            .accept(
                early.clone(),
                &mut node.utxo,
                &mut node.mempool,
                node.now,
                &TESTNET
            )
            .is_err());

        assert!(
            node.chain
                .to_check(&early, &node.utxo, node.now + 100_000, &TESTNET)
                .is_none(),
            "the two paths must not disagree about the same body"
        );
    }

    /// A reorg is not this path's business: it re-validates a whole branch
    /// against a set that moves beneath it, and the re-check that makes the
    /// optimistic path sound does not transfer.
    #[test]
    fn a_block_that_does_not_extend_the_tip_is_not_checked_without_the_lock() {
        let mut node = a_node();
        let root = node.chain.tip();
        let first = node.candidate(Vec::new());
        node.connect(first).unwrap();

        let sibling = node.branch(root, 1, 9).remove(0);

        assert!(
            node.chain
                .to_check(&sibling, &node.utxo, node.now, &TESTNET)
                .is_none(),
            "a branch takes the path that holds the lock"
        );
    }

    /// The optimistic path and the ordinary one must land in the same place.
    #[test]
    fn a_vouched_block_lands_where_the_ordinary_path_would_have_put_it() {
        let mut ours = a_node();
        let mut theirs = a_node();
        let key = PrivateKey::random();
        let outpoint = funded(&mut ours.utxo, &key, 1_000, 0);
        funded(&mut theirs.utxo, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let block = ours.candidate(vec![payment]);

        let vouched = ours
            .chain
            .to_check(&block, &ours.utxo, ours.now, &TESTNET)
            .unwrap()
            .vouch(&block, &TESTNET)
            .unwrap();
        ours.chain
            .accept_vouched(
                block.clone(),
                &vouched,
                &mut ours.utxo,
                &mut ours.mempool,
                ours.now,
                &TESTNET,
            )
            .unwrap();
        theirs.connect(block).unwrap();

        assert_eq!(ours.chain.tip(), theirs.chain.tip());
        assert_eq!(ours.chain.height(), theirs.chain.height());
        assert_eq!(ours.coins(), theirs.coins());
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
        // Nonces until one does not solve it. Bumping by one is a coin flip at
        // this difficulty, and a test that passes 255 times in 256 is worse
        // than none.
        while broken.header().unwrap().meets_its_target().unwrap() {
            broken.nonce = broken.nonce.wrapping_add(1);
        }

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
            let outcome =
                self.chain
                    .accept(block, &mut self.utxo, &mut self.mempool, self.now, &TESTNET);
            self.readmit();

            outcome
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
    fn a_block_nobody_mined_is_not_worth_holding() {
        let mut node = a_node();
        let root = node.chain.tip();
        let mut unmined = node.branch(root, 2, 1).remove(1);
        // A header nobody paid for, found rather than assumed.
        while unmined.header().unwrap().meets_its_target().unwrap() {
            unmined.nonce = unmined.nonce.wrapping_add(1);
        }

        assert!(node.accept(unmined).is_err());
        assert!(
            node.chain.orphans.is_empty(),
            "orphaning it would let a peer fill memory for free"
        );
    }

    #[test]
    fn a_block_arriving_at_a_full_orphan_pool_is_not_reported_as_waiting() {
        let mut node = a_node();
        let root = node.chain.tip();
        let branch = node.branch(root, MAX_ORPHANS as u32 + 2, 1);

        let mut last = Accepted::Held(root);
        for block in branch.iter().skip(1) {
            last = node.accept(block.clone()).unwrap();
        }

        assert!(
            matches!(last, Accepted::Held(_)),
            "a caller that heard `Orphaned` would ask for a parent we did not keep"
        );
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
    fn a_locator_names_the_chain_in_far_fewer_hashes_than_it_has_blocks() {
        let mut node = a_node();
        let root = node.chain.tip();
        for block in node.branch(root, 40, 1) {
            node.accept(block).unwrap();
        }

        let locator = node.chain.locator();

        assert!(locator.len() < 20, "{} hashes for 41 blocks", locator.len());
        assert_eq!(locator[0], node.chain.tip(), "newest first");
        assert_eq!(*locator.last().unwrap(), root, "and genesis last");
    }

    #[test]
    fn headers_are_offered_from_where_the_two_chains_last_agreed() {
        let mut node = a_node();
        let root = node.chain.tip();
        let branch = node.branch(root, 6, 1);
        for block in &branch {
            node.accept(block.clone()).unwrap();
        }

        let behind = branch[1].header().unwrap().hash();
        let offered = node
            .chain
            .headers_after(&[behind], &BlockHash::from_bytes([0; 32]));

        assert_eq!(offered.len(), 4, "everything after the one they named");
        assert_eq!(offered[0].previous_block_hash, behind);
    }

    /// A locator always ends at genesis, so answering from the *first* match
    /// answers every request from height one — and a peer past `MAX_HEADERS`
    /// never makes progress. This is the case a single-hash locator cannot
    /// catch.
    #[test]
    fn a_locator_is_answered_from_the_newest_block_the_two_chains_share() {
        let mut node = a_node();
        let root = node.chain.tip();
        let branch = node.branch(root, 8, 1);
        for block in &branch {
            node.accept(block.clone()).unwrap();
        }

        // Genesis last, as a real locator has it.
        let locator = vec![branch[4].header().unwrap().hash(), root];
        let offered = node
            .chain
            .headers_after(&locator, &BlockHash::from_bytes([0; 32]));

        assert_eq!(
            offered.len(),
            3,
            "everything after the fifth, not after genesis"
        );
        assert_eq!(offered[0].previous_block_hash, locator[0]);
    }

    #[test]
    fn a_block_off_the_best_chain_is_not_a_place_to_answer_from() {
        let mut node = a_node();
        let root = node.chain.tip();
        let losing = node.branch(root, 1, 1);
        let winning = node.branch(root, 3, 2);
        node.accept(losing[0].clone()).unwrap();
        for block in &winning {
            node.accept(block.clone()).unwrap();
        }

        let orphaned_tip = losing[0].header().unwrap().hash();

        assert!(node.chain.index().height_on_best(&orphaned_tip).is_none());
        assert_eq!(
            node.chain
                .headers_after(&[orphaned_tip, root], &BlockHash::from_bytes([0; 32]))
                .len(),
            3,
            "from genesis, because nothing they named is on our chain"
        );
    }

    #[test]
    fn a_switch_moves_the_best_chain_it_answers_from() {
        let mut node = a_node();
        let root = node.chain.tip();
        let losing = node.branch(root, 1, 1);
        let winning = node.branch(root, 2, 2);
        node.accept(losing[0].clone()).unwrap();
        assert_eq!(node.chain.index().best_chain().len(), 2);

        for block in &winning {
            node.accept(block.clone()).unwrap();
        }

        assert_eq!(node.chain.index().best_chain().len(), 3);
        assert_eq!(
            *node.chain.index().best_chain().last().unwrap(),
            winning[1].header().unwrap().hash()
        );
    }

    #[test]
    fn a_locator_naming_nothing_we_know_is_answered_from_genesis() {
        let mut node = a_node();
        let root = node.chain.tip();
        for block in node.branch(root, 3, 1) {
            node.accept(block).unwrap();
        }

        let offered = node.chain.headers_after(
            &[BlockHash::from_bytes([9; 32])],
            &BlockHash::from_bytes([0; 32]),
        );

        assert_eq!(offered.len(), 4, "genesis and everything after it");
    }

    #[test]
    fn a_header_alone_is_enough_to_learn_a_branch_without_its_bodies() {
        let mut node = a_node();
        let root = node.chain.tip();
        let branch = node.branch(root, 3, 1);

        for block in &branch {
            node.chain
                .add_header(block.header().unwrap(), node.now, &TESTNET)
                .unwrap();
        }

        assert_eq!(node.chain.height(), 0, "no body, so no tip movement");
        assert_eq!(
            node.chain.bodies_wanted(10),
            branch
                .iter()
                .map(|block| block.header().unwrap().hash())
                .collect::<Vec<_>>(),
            "and each of them is worth asking for, oldest first"
        );
    }

    #[test]
    fn a_header_nobody_mined_is_not_recorded() {
        let mut node = a_node();
        let root = node.chain.tip();
        let mut unmined = node.branch(root, 1, 1).remove(0);
        while unmined.header().unwrap().meets_its_target().unwrap() {
            unmined.nonce = unmined.nonce.wrapping_add(1);
        }

        assert!(node
            .chain
            .add_header(unmined.header().unwrap(), node.now, &TESTNET)
            .is_err());
    }

    #[test]
    fn a_header_that_connects_to_nothing_is_refused() {
        let mut node = a_node();
        let stranded = node.branch(node.chain.tip(), 2, 1).remove(1);

        assert!(node
            .chain
            .add_header(stranded.header().unwrap(), node.now, &TESTNET)
            .is_err());
    }

    #[test]
    fn a_header_from_the_future_is_refused_before_its_body_is_asked_for() {
        let mut node = a_node();
        let root = node.chain.tip();
        let mut early = node.branch(root, 1, 1).remove(0);
        early.time = node.now + crate::difficulty::MAX_FUTURE_DRIFT + 60;
        assert!(early.mine().unwrap());

        assert!(node
            .chain
            .add_header(early.header().unwrap(), node.now, &TESTNET)
            .is_err());
        assert!(node.chain.bodies_wanted(10).is_empty());
    }

    /// The header-level rule, which nothing pinned: a header whose time is
    /// not past the median of the last eleven is refused before its body is
    /// worth asking for. `validation.rs` covers the same rule for a block;
    /// this covers it for a header, which is where it saves the fetch.
    #[test]
    fn a_header_not_past_the_median_is_refused_and_its_body_is_not_asked_for() {
        let mut node = a_node();
        for _ in 0..MEDIAN_TIME_SPAN + 1 {
            let next = node.candidate(Vec::new());
            node.connect(next).unwrap();
        }

        let root = node.chain.tip();
        let median = node.chain.index().median_time_after(&root).unwrap();
        let mut stale = node.branch(root, 1, 7).remove(0);
        stale.time = median;
        assert!(stale.search(0, u32::MAX).is_some());
        stale.nonce = stale.search(0, u32::MAX).unwrap();
        stale.seal().unwrap();

        let refusal = node
            .chain
            .add_header(stale.header().unwrap(), node.now, &TESTNET)
            .expect_err("a header at the median is not past it");

        assert!(format!("{refusal:#}").contains("median"), "{refusal:#}");
        assert!(node.chain.bodies_wanted(10).is_empty());
    }

    #[test]
    fn headers_that_connect_to_nothing_leave_the_node_as_they_found_it() {
        let mut node = a_node();
        let stranded = node.branch(node.chain.tip(), 3, 1);
        let before = node.chain.index().len();

        for block in stranded.iter().skip(1) {
            let _ = node
                .chain
                .add_header(block.header().unwrap(), node.now, &TESTNET);
        }

        assert_eq!(node.chain.index().len(), before, "nothing kept");
        assert_eq!(node.chain.bodies_missing(), 0, "and nothing to fetch");
    }

    #[test]
    fn how_far_behind_we_are_is_the_bodies_we_have_not_got() {
        let mut node = a_node();
        let root = node.chain.tip();
        let branch = node.branch(root, 4, 1);
        assert_eq!(node.chain.bodies_missing(), 0);

        for block in &branch {
            node.chain
                .add_header(block.header().unwrap(), node.now, &TESTNET)
                .unwrap();
        }
        assert_eq!(node.chain.bodies_missing(), 4);

        node.accept(branch[0].clone()).unwrap();

        assert_eq!(node.chain.bodies_missing(), 3);
    }

    #[test]
    fn a_body_already_held_is_not_asked_for_again() {
        let mut node = a_node();
        let root = node.chain.tip();
        let branch = node.branch(root, 2, 1);
        node.accept(branch[0].clone()).unwrap();
        node.chain
            .add_header(branch[1].header().unwrap(), node.now, &TESTNET)
            .unwrap();

        assert_eq!(
            node.chain.bodies_wanted(10),
            vec![branch[1].header().unwrap().hash()]
        );
    }

    /// The attack ADR-0010 chose Bitcoin's remedy against, and the half of it
    /// that is easy to leave open: `[a, b, c]` and `[a, b, c, c]` collapse to
    /// the same merkle root, so a legitimate block and a malformed one share a
    /// hash. Refusing the malformed one must not refuse the other.
    #[test]
    fn a_block_refused_for_a_duplicated_transaction_does_not_poison_its_hash() {
        let mut node = a_node();
        let key = PrivateKey::random();
        let first = funded(&mut node.utxo, &key, 1_000, 0);
        let second = funded(&mut node.utxo, &key, 2_000, 0);
        let root = node.chain.tip();

        let payments = vec![
            signed(&key, &[first], vec![pay_to(&key, 900)]),
            signed(&key, &[second], vec![pay_to(&key, 1_900)]),
        ];
        let honest = node.candidate_on(root, payments, 1);
        let hash = honest.header().unwrap().hash();

        // The same header, with the last transaction repeated. Three leaves
        // and four pair to the same root, so this is the same block hash.
        let mut poisoned = honest.clone();
        poisoned
            .transactions
            .push(honest.transactions.last().unwrap().clone());
        // The premise, checked where it can be: the two bodies really do
        // produce one merkle root, so a header committing to it commits to
        // neither of them in particular. `Block::header` reads the cached
        // root, so asserting on that would prove nothing.
        assert_eq!(
            merkle_root(
                &honest
                    .transactions
                    .iter()
                    .map(|t| *t.get_wtxid().as_bytes())
                    .collect::<Vec<_>>()
            ),
            merkle_root(
                &poisoned
                    .transactions
                    .iter()
                    .map(|t| *t.get_wtxid().as_bytes())
                    .collect::<Vec<_>>()
            ),
        );

        assert!(node.accept(poisoned).is_err());
        assert!(
            !node.chain.failed.contains(&hash),
            "recording it would refuse the honest block forever"
        );

        assert_eq!(
            node.accept(honest).unwrap(),
            Accepted::Extended(hash),
            "and the honest block, arriving second, is taken"
        );
    }

    /// A block's hash commits to its header, and the header to a merkle root.
    /// A body that does not match that root is not *the* body behind the hash
    /// — some other body is — so refusing this one must not refuse that one.
    #[test]
    fn a_block_whose_body_does_not_match_its_root_does_not_poison_its_hash() {
        let mut node = a_node();
        let root = node.chain.tip();
        let honest = node.candidate_on(root, Vec::new(), 1);
        let hash = honest.header().unwrap().hash();

        let mut swapped = honest.clone();
        swapped.transactions = vec![Transaction::coinbase(
            1,
            99,
            vec![pay_to(&PrivateKey::random(), subsidy(1).atoms())],
        )];

        assert!(node.accept(swapped).is_err());
        assert!(!node.chain.failed.contains(&hash));
        assert_eq!(node.accept(honest).unwrap(), Accepted::Extended(hash));
    }

    #[test]
    fn the_same_refused_body_is_not_checked_twice() {
        let mut node = a_node();
        let root = node.chain.tip();
        let honest = node.candidate_on(root, Vec::new(), 1);
        let mut swapped = honest.clone();
        swapped.transactions = vec![Transaction::coinbase(
            1,
            99,
            vec![pay_to(&PrivateKey::random(), subsidy(1).atoms())],
        )];

        assert!(node.accept(swapped.clone()).is_err());

        let again = format!("{:#}", node.accept(swapped).unwrap_err());

        assert!(
            again.contains("already refused"),
            "a resend costs a lookup, not a merkle root: {again}"
        );
    }

    #[test]
    fn a_block_refused_for_a_sixty_four_byte_transaction_is_remembered() {
        let mut node = a_node();
        let root = node.chain.tip();
        let mut block = node.candidate_on(root, Vec::new(), 1);

        // The smallest transaction is 53 bytes; eleven of script_pubkey makes
        // it exactly the size of a merkle node.
        let filler = Transaction {
            version: 1,
            inputs: vec![crate::transaction::TxIn {
                previous_output: Outpoint::null(),
                coinbase_data: Vec::new(),
                witness: crate::transaction::Witness::empty(),
            }],
            outputs: vec![crate::transaction::TxOut {
                value: Amount::from_atoms(1).unwrap(),
                script_pubkey: vec![0; 11],
            }],
        };
        assert_eq!(filler.get_raw_format().len(), 64);
        // Pushed after mining: a block carrying one has no merkle root, so it
        // is not something this code could mine in the first place.
        block.transactions.push(filler);
        let hash = block.header().unwrap().hash();

        assert!(node.accept(block).is_err());
        assert!(
            node.chain.failed.contains(&hash),
            "its root does cover this body, so remembering it is right"
        );
    }

    #[test]
    fn every_other_refusal_is_still_remembered() {
        let mut node = a_node();
        let root = node.chain.tip();
        let mut overpaying = node.candidate_on(root, Vec::new(), 1);
        overpaying.transactions[0].outputs[0].value =
            Amount::from_atoms(subsidy(1).atoms() + 1).unwrap();
        assert!(overpaying.mine().unwrap());
        let hash = overpaying.header().unwrap().hash();

        assert!(node.accept(overpaying).is_err());
        assert!(
            node.chain.failed.contains(&hash),
            "the exception is narrow: a body its hash does cover stays refused"
        );
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
