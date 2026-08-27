use crate::block::{BlockHash, Header};
use crate::difficulty::{median_time_past, required_bits, RETARGET_WINDOW};
use crate::params::Network;
use anyhow::{anyhow, bail, Result};
use primitive_types::U256;
use std::collections::HashMap;

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
    /// Which of two equally-heavy tips arrived first.
    seen: u64,
}

/// Every header the node has accepted, and which of them is the best chain.
/// Tolerates more than one tip: two miners racing is the normal case, not an
/// error, and the node holds both until one branch outweighs the other.
#[derive(Debug)]
pub struct BlockIndex {
    entries: HashMap<BlockHash, Entry>,
    best: BlockHash,
    arrivals: u64,
}

impl BlockIndex {
    pub fn new(genesis: Header) -> Result<Self> {
        let hash = genesis.hash();
        let entry = Entry {
            header: genesis,
            height: 0,
            total_work: genesis.work()?,
            parent: None,
            seen: 0,
        };

        Ok(BlockIndex {
            entries: HashMap::from([(hash, entry)]),
            best: hash,
            arrivals: 1,
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
            seen: self.arrivals,
        };

        self.arrivals += 1;
        let outweighs = self.is_better(&entry);
        self.entries.insert(hash, entry);
        if outweighs {
            self.best = hash;
        }

        Ok(hash)
    }

    // Strictly greater, so an equal-work tip does not displace one already
    // held: first seen wins, and a peer cannot take the tip by being loud.
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
        let mut claimed: Vec<BlockHash> = self.entries.values().filter_map(|e| e.parent).collect();
        claimed.sort_unstable();

        let mut tips: Vec<BlockHash> = self
            .entries
            .keys()
            .filter(|hash| claimed.binary_search(hash).is_err())
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

    /// The median of the eleven blocks up to and including `parent`.
    pub fn median_time_after(&self, parent: &BlockHash) -> Result<u32> {
        median_time_past(&self.timestamps_to(parent, 11))
            .ok_or_else(|| anyhow!("{parent} is not a block this node knows"))
    }

    /// The fork point of two branches, and the entries to disconnect and
    /// connect to move from one to the other.
    pub fn fork_point(&self, from: &BlockHash, to: &BlockHash) -> Result<BlockHash> {
        let mut left = *from;
        let mut right = *to;

        loop {
            let (a, b) = (self.expect(&left)?, self.expect(&right)?);
            if left == right {
                return Ok(left);
            }

            if a.height >= b.height {
                left = a.parent.ok_or_else(bail_no_fork)?;
            } else {
                right = b.parent.ok_or_else(bail_no_fork)?;
            }
        }
    }

    fn expect(&self, hash: &BlockHash) -> Result<&Entry> {
        self.get(hash)
            .ok_or_else(|| anyhow!("{hash} is not a block this node knows"))
    }
}

fn bail_no_fork() -> anyhow::Error {
    anyhow!("two branches with no common ancestor: they are not the same chain")
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

    #[test]
    fn two_branches_fork_where_they_last_agreed() {
        let mut index = an_index();
        let start = index.best_hash();
        let root = extend(&mut index, start, EASY, 3, 1);
        let left = extend(&mut index, root, EASY, 4, 2);
        let right = extend(&mut index, root, EASY, 2, 3);

        assert_eq!(index.fork_point(&left, &right).unwrap(), root);
        assert_eq!(index.fork_point(&left, &left).unwrap(), left);
    }

    #[test]
    fn a_block_nobody_has_heard_of_has_no_fork_point() {
        let index = an_index();
        let stranger = BlockHash::from_bytes([7; 32]);

        assert!(index.fork_point(&index.best_hash(), &stranger).is_err());
    }
}
