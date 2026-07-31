use crate::byte_reader::ByteReader;
use crate::transaction::Transaction;
use crate::util::{get_compact_int, get_hash};
use anyhow::{anyhow, Context, Result};
use primitive_types::U256;

/// Bitcoin's merkle construction: pair left to right, level by level,
/// duplicating the last node wherever a level has an odd count.
///
/// Each level is built into a new vector rather than pushed back onto the one
/// being read — doing the latter feeds a level's own results back into it and
/// produces a chain, not a tree.
fn merkle_root(leaves: &[[u8; 32]]) -> Option<[u8; 32]> {
    if leaves.is_empty() {
        return None;
    }

    let mut level = leaves.to_vec();

    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| {
                let mut joined = [0u8; 64];
                joined[..32].copy_from_slice(&pair[0]);
                joined[32..].copy_from_slice(pair.get(1).unwrap_or(&pair[0]));
                get_hash(&joined)
            })
            .collect();
    }

    Some(level[0])
}

#[derive(Clone, Debug)]
pub struct Block {
    pub version: i32,
    pub previous_block_hash: [u8; 32],
    pub merkle_root_hash: Option<[u8; 32]>,
    pub time: u32,
    pub n_bits: u32,
    pub nonce: u32,
    pub hash: Option<[u8; 32]>,
    mine_array: [u8; 80],
    pub transactions: Vec<Transaction>,
}

impl Block {
    pub fn new(
        version: i32,
        previous_block_hash: [u8; 32],
        time: u32,
        n_bits: u32,
        transactions: Vec<Transaction>,
    ) -> Self {
        Block {
            version,
            previous_block_hash,
            merkle_root_hash: None,
            time,
            n_bits,
            nonce: 0,
            hash: None,
            mine_array: [0; 80],
            transactions,
        }
    }

    pub fn mine(&mut self) -> Result<bool> {
        self.merkle_root_hash = Some(self.get_merkle_root_hash()?);

        self.prepare_for_mining()?;

        let n_bits = self.get_target_256();

        for nonce in 0..u32::MAX {
            self.mine_array[76..80].copy_from_slice(&nonce.to_le_bytes());
            let hash = get_hash(self.mine_array.as_slice());
            let hash256 = U256::from_little_endian(&hash);
            if hash256 < n_bits {
                self.nonce = nonce;
                self.hash = Some(hash);
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn prepare_for_mining(&mut self) -> Result<()> {
        self.mine_array[0..4].copy_from_slice(&self.version.to_le_bytes());

        self.mine_array[4..36].copy_from_slice(&self.previous_block_hash);

        self.mine_array[36..68].copy_from_slice(
            &self
                .merkle_root_hash
                .context("Merkle root is required to mine")?,
        );

        self.mine_array[68..72].copy_from_slice(&self.time.to_le_bytes());

        self.mine_array[72..76].copy_from_slice(&self.n_bits.to_le_bytes());

        self.mine_array[76..80].copy_from_slice(&self.nonce.to_le_bytes());

        Ok(())
    }

    fn get_target_256(&self) -> U256 {
        let target: u32 = self.n_bits;
        let exponent = target >> 24;
        let mantissa = target & 0x007FFFFF;

        let target = U256::from(mantissa);
        target << (exponent * 8)
    }

    fn get_merkle_root_hash(&self) -> Result<[u8; 32]> {
        let leaves: Vec<[u8; 32]> = self.transactions.iter().map(|tx| tx.get_tx_id()).collect();

        merkle_root(&leaves).context("a block needs a transaction to have a merkle root")
    }

    pub fn get_raw_format(&self) -> Result<Vec<u8>> {
        if self.hash.is_none() {
            return Err(anyhow!(
                "Hash is empty, you need to mine or assign a hash to the block"
            ));
        }
        let mut raw_format = Vec::new();

        raw_format.extend(&self.mine_array);

        raw_format.extend(get_compact_int(self.transactions.len() as u64));

        for tx in &self.transactions {
            raw_format.extend(tx.get_raw_format());
        }

        Ok(raw_format)
    }

    pub(crate) fn parse_raw(bytes: Vec<u8>) -> Result<Block> {
        let mut reader = ByteReader::new(&bytes);
        let version = reader.read_i32()?;
        let previous_block_hash = reader.read_array::<32>()?;
        let merkle_root_hash = Some(reader.read_array::<32>()?);
        let time = reader.read_u32()?;
        let n_bits = reader.read_u32()?;
        let nonce = reader.read_u32()?;
        let tx_count = reader.read_compact()?;

        let mut transactions = Vec::with_capacity(tx_count as usize);
        for _ in 0..tx_count {
            transactions.push(Transaction::parse_raw(&mut reader)?);
        }

        let block = Self {
            version,
            previous_block_hash,
            merkle_root_hash,
            time,
            n_bits,
            nonce,
            hash: None,
            mine_array: [0; 80],
            transactions,
        };

        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{Outpoint, TxIn, TxOut};
    use hex::{decode, encode};
    use primitive_types::U256;
    use rstest::rstest;

    /// Hashes are little-endian internally and only reversed for display, so a
    /// txid copied from a block explorer has to be reversed to be used here.
    fn leaf(displayed: &str) -> [u8; 32] {
        let mut bytes: [u8; 32] = decode(displayed).unwrap().try_into().unwrap();
        bytes.reverse();
        bytes
    }

    fn displayed(root: [u8; 32]) -> String {
        let mut bytes = root;
        bytes.reverse();
        encode(bytes)
    }

    fn node(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
        get_hash(&[left, right].concat())
    }

    #[test]
    fn the_root_of_a_real_blocks_transactions_matches_its_published_root() {
        // Bitcoin block 170 — the first payment ever made, Satoshi to Hal
        // Finney. Two transactions, so this pins pair order as well as hashing.
        let coinbase = leaf("b1fea52486ce0c62bb442b530a3f0132b826c74e473d1f2c220bfa78111c5082");
        let payment = leaf("f4184fc596403b9d638783cf57adfe4c75c605f6356fbc91338530e9831e9e16");

        assert_eq!(
            "7dac2c5666815c17a3b36427de37bb9d2e2c5ccec3f8633eb91a4205cb4c10ff",
            displayed(merkle_root(&[coinbase, payment]).unwrap())
        );
    }

    #[test]
    fn a_single_transaction_is_its_own_root() {
        // Bitcoin's genesis block: one transaction, and its merkle root is that
        // transaction's id unchanged.
        let coinbase = leaf("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b");

        assert_eq!(coinbase, merkle_root(&[coinbase]).unwrap());
    }

    #[test]
    fn four_leaves_pair_left_to_right() {
        let [a, b, c, d] = [
            leaf_number(1),
            leaf_number(2),
            leaf_number(3),
            leaf_number(4),
        ];

        assert_eq!(
            node(node(a, b), node(c, d)),
            merkle_root(&[a, b, c, d]).unwrap(),
            "the root must be H(H(a,b),H(c,d)), not a chain"
        );
    }

    #[test]
    fn an_odd_level_duplicates_its_last_node() {
        let [a, b, c] = [leaf_number(1), leaf_number(2), leaf_number(3)];

        assert_eq!(
            node(node(a, b), node(c, c)),
            merkle_root(&[a, b, c]).unwrap()
        );
    }

    #[test]
    fn duplication_happens_per_level_not_by_padding_the_leaves() {
        // Six leaves separate the two: per-level gives H(H(ab,cd), H(ef,ef)),
        // while padding the leaf list out to eight gives H(H(ab,cd), H(ef,ff)).
        let leaves: Vec<[u8; 32]> = (1..=6).map(leaf_number).collect();
        let [a, b, c, d, e, f] = <[[u8; 32]; 6]>::try_from(leaves.clone()).unwrap();

        let per_level = node(node(node(a, b), node(c, d)), node(node(e, f), node(e, f)));
        let padded = node(node(node(a, b), node(c, d)), node(node(e, f), node(f, f)));

        assert_ne!(per_level, padded, "the two constructions must differ here");
        assert_eq!(per_level, merkle_root(&leaves).unwrap());
    }

    #[test]
    fn no_transactions_has_no_root() {
        assert_eq!(None, merkle_root(&[]));
    }

    fn leaf_number(n: u8) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        bytes
    }

    #[rstest]
    #[case(1usize)]
    #[case(2usize)]
    #[case(3usize)]
    #[case(4usize)]
    fn mines_generates_correct_hash(#[case] number_of_transactions: usize) {
        let mut block = get_block(number_of_transactions);

        // Only asserts that a nonce was found: any merkle root satisfies this,
        // so the root itself is pinned by the tests above, not by mining.
        assert!(block.mine().unwrap());
    }

    #[test]
    fn block_generates_correct_hash() {
        let mut block = get_block(2);

        block.prepare_for_mining().unwrap();

        let hash = get_hash(block.mine_array.as_slice());

        let hash = U256::from_little_endian(&hash);
        let target = block.get_target_256();

        assert!(hash < target, "Hash should be lesser than target");

        assert_eq!(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
            encode(hash.to_big_endian()),
            "Block hash is wrong"
        )
    }

    #[test]
    fn pre_hash_correctly_assembled() {
        let mut block = Block {
            version: 1,
            previous_block_hash: [0u8; 32],
            merkle_root_hash: Some(
                decode("3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a")
                    .unwrap()
                    .try_into()
                    .unwrap(),
            ),
            time: 0x495fab29,
            n_bits: 0x1d00ffff,
            nonce: 0,
            hash: None,
            mine_array: [0; 80],
            transactions: vec![],
        };

        block.prepare_for_mining().unwrap();

        let expected_previous_block_hash =
            decode("0000000000000000000000000000000000000000000000000000000000000000").unwrap();
        let expected_merkle_root_hash =
            decode("3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a").unwrap();

        assert_eq!(
            block.mine_array.len(),
            80,
            "mine_array length does not match expected length"
        );

        assert_eq!(
            &block.mine_array[0..4],
            0x01000000u32.to_be_bytes(),
            "Version part does not match"
        );

        assert_eq!(
            &block.mine_array[4..36],
            expected_previous_block_hash,
            "Previous block hash part does not match"
        );

        assert_eq!(
            &block.mine_array[36..68],
            expected_merkle_root_hash,
            "Merkle root hash part does not match"
        );

        assert_eq!(
            &block.mine_array[68..72],
            0x29ab5f49u32.to_be_bytes(),
            "Time part does not match"
        );

        assert_eq!(
            &block.mine_array[72..76],
            0xffff001du32.to_be_bytes(),
            "n_bits part does not match"
        );
    }
    #[test]
    fn test_serialization_and_deserialization() {
        let mut original_block = get_block(3);

        assert!(original_block.mine().unwrap());

        let raw_data = original_block.get_raw_format().unwrap();

        let parsed_block = Block::parse_raw(raw_data).unwrap();

        assert_eq!(
            original_block.version, parsed_block.version,
            "Version should match"
        );
        assert_eq!(
            original_block.previous_block_hash, parsed_block.previous_block_hash,
            "Previous block hash should match"
        );
        assert_eq!(
            original_block.merkle_root_hash, parsed_block.merkle_root_hash,
            "Merkle root hash should match"
        );
        assert_eq!(original_block.time, parsed_block.time, "Time should match");
        assert_eq!(
            original_block.n_bits, parsed_block.n_bits,
            "n_bits should match"
        );
        assert_eq!(
            original_block.nonce, parsed_block.nonce,
            "Nonce should match"
        );

        assert_eq!(
            original_block.transactions.len(),
            parsed_block.transactions.len(),
            "Number of transactions should match"
        );

        for (i, (original_tx, parsed_tx)) in original_block
            .transactions
            .iter()
            .zip(parsed_block.transactions.iter())
            .enumerate()
        {
            assert_eq!(
                original_tx.version, parsed_tx.version,
                "Transaction {} version should match",
                i
            );
        }
    }
    fn get_tx() -> Transaction {
        Transaction {
            version: 1,
            inputs: {
                vec![TxIn {
                    previous_output: {
                        Outpoint {
                            tx_id: [0; 32],
                            v_out: 0,
                        }
                    },
                    signature: "my_signature".to_string(),
                    sequence: 0xFFFFFFFF,
                }]
            },
            outputs: vec![TxOut {
                value: 10_000,
                destiny_pub_key: "12345".to_string(),
            }],
            lock_time: 0,
        }
    }

    fn get_block(number_of_transactions: usize) -> Block {
        Block {
            version: 1,
            previous_block_hash: [0u8; 32],
            merkle_root_hash: Some(
                decode("3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a")
                    .unwrap()
                    .try_into()
                    .unwrap(),
            ),
            time: 0x495fab29,
            n_bits: 0x1d00ffff,
            nonce: 0x7c2bac1d,
            hash: None,
            mine_array: [0; 80],
            transactions: vec![get_tx(); number_of_transactions],
        }
    }
}
