use crate::byte_reader::ByteReader;
use crate::transaction::{Transaction, Wtxid, MIN_TRANSACTION_SIZE};
use crate::util::{get_compact_int, get_hash};
use anyhow::{anyhow, Context, Result};
use primitive_types::U256;
use std::collections::HashSet;

/// Two concatenated 32-byte children. A transaction serializing to exactly this
/// is indistinguishable from an internal node — ADR-0019.
const MERKLE_NODE_SIZE: usize = 64;

/// Compact form is `mantissa × 256^(exponent − 3)`, so the three bytes the
/// mantissa already occupies are not shifted again. `n_bits` arrives inside a
/// header a stranger sent, so every way it can fail to name a target is a
/// refusal rather than a value.
///
/// The sign bit is refused whatever the rest of the mantissa holds. Bitcoin
/// calls `0x00800000` positive zero and decodes it to a target of zero; we
/// refuse it, because a compact number carrying a sign is not a target and
/// there is nothing here that needs the distinction.
pub fn target_from_bits(n_bits: u32) -> Result<U256> {
    let exponent = n_bits >> 24;
    let mantissa = n_bits & 0x00ff_ffff;

    if mantissa & 0x0080_0000 != 0 {
        return Err(anyhow!("n_bits {n_bits:#010x} is negative"));
    }

    let shift = match exponent.checked_sub(3) {
        Some(bytes) => bytes,
        None => {
            let truncated = mantissa >> (8 * (3 - exponent));
            return Ok(U256::from(truncated));
        }
    };

    if mantissa != 0 && shift * 8 + 32 - mantissa.leading_zeros() > 256 {
        return Err(anyhow!(
            "n_bits {n_bits:#010x} names a target over 256 bits"
        ));
    }

    Ok(U256::from(mantissa) << (shift * 8))
}

/// The inverse of `target_from_bits`, rounding down so the encoded target is
/// never easier than the one asked for.
pub fn bits_from_target(target: U256) -> u32 {
    if target.is_zero() {
        return 0;
    }

    let mut exponent = (target.bits() as u32).div_ceil(8);
    let mut mantissa = if exponent <= 3 {
        (target.low_u64() << (8 * (3 - exponent))) as u32
    } else {
        (target >> (8 * (exponent - 3))).low_u32()
    };

    // The top bit of the mantissa is the sign, so a mantissa that would set it
    // is carried into the exponent instead.
    if mantissa & 0x0080_0000 != 0 {
        mantissa >>= 8;
        exponent += 1;
    }

    (exponent << 24) | mantissa
}

fn merkle_root(leaves: &[[u8; 32]]) -> Option<[u8; 32]> {
    if leaves.is_empty() {
        return None;
    }

    let mut level = leaves.to_vec();

    // A new vector per level: pushing back into the one being read feeds a
    // level's results into itself and builds a chain, not a tree.
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

        let target = target_from_bits(self.n_bits)?;

        for nonce in 0..u32::MAX {
            self.mine_array[76..80].copy_from_slice(&nonce.to_le_bytes());
            let hash = get_hash(self.mine_array.as_slice());
            let hash256 = U256::from_little_endian(&hash);
            if hash256 < target {
                self.nonce = nonce;
                self.hash = Some(hash);
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Fills in the header from the nonce already set, and refuses if that
    /// nonce does not solve it. This is how a committed nonce is checked —
    /// `mine` searches for one, `seal` is handed one.
    pub fn seal(&mut self) -> Result<()> {
        self.merkle_root_hash = Some(self.get_merkle_root_hash()?);
        self.prepare_for_mining()?;

        let hash = get_hash(&self.mine_array);
        if U256::from_little_endian(&hash) >= target_from_bits(self.n_bits)? {
            return Err(anyhow!("nonce {} does not meet the target", self.nonce));
        }

        self.hash = Some(hash);
        Ok(())
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

    fn get_merkle_root_hash(&self) -> Result<[u8; 32]> {
        let mut seen: HashSet<Wtxid> = HashSet::new();
        let mut leaves = Vec::with_capacity(self.transactions.len());

        for transaction in &self.transactions {
            let raw = transaction.get_raw_format();
            if raw.len() == MERKLE_NODE_SIZE {
                return Err(anyhow!(
                    "a transaction of {MERKLE_NODE_SIZE} bytes is invalid: ADR-0019"
                ));
            }

            // Serialized once for both; `a_blocks_leaves_are_its_wtxids_in_order`
            // is what keeps this equal to `get_wtxid`.
            let wtxid = Wtxid::from_bytes(get_hash(&raw));
            if !seen.insert(wtxid) {
                return Err(anyhow!("two transactions share the wtxid {wtxid}"));
            }

            leaves.push(*wtxid.as_bytes());
        }

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
        let mut transactions = Vec::new();
        for _ in 0..reader.read_count(MIN_TRANSACTION_SIZE)? {
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
    use crate::amount::Amount;
    use crate::transaction::{Outpoint, TxIn, TxOut, Txid, Witness};
    use hex::{decode, encode};
    use primitive_types::U256;
    use rstest::rstest;

    // Hashes are little-endian internally, so a txid copied from an explorer
    // has to be reversed to be used here.
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

    /// Published compact-target vectors: Bitcoin's proof-of-work limit, the
    /// worked example in its own difficulty documentation, two real block
    /// headers, and the three small cases that reach the sub-3 exponent path.
    #[rstest]
    #[case::bitcoin_proof_of_work_limit(
        0x1d00ffff,
        "00000000ffff0000000000000000000000000000000000000000000000000000"
    )]
    #[case::the_published_worked_example(
        0x1b0404cb,
        "00000000000404cb000000000000000000000000000000000000000000000000"
    )]
    #[case::block_277_316(
        0x1903a30c,
        "0000000000000003a30c00000000000000000000000000000000000000000000"
    )]
    #[case::block_500_000(
        0x18009645,
        "0000000000000000009645000000000000000000000000000000000000000000"
    )]
    #[case::exponent_three(
        0x03000001,
        "0000000000000000000000000000000000000000000000000000000000000001"
    )]
    #[case::exponent_below_three(
        0x02008000,
        "0000000000000000000000000000000000000000000000000000000000000080"
    )]
    #[case::mantissa_shifted_away(
        0x01003456,
        "0000000000000000000000000000000000000000000000000000000000000000"
    )]
    fn n_bits_decodes_to_its_published_target(#[case] n_bits: u32, #[case] expected: &str) {
        assert_eq!(
            format!("{:064x}", target_from_bits(n_bits).unwrap()),
            expected
        );
    }

    #[rstest]
    #[case(0x1d00ffff)]
    #[case(0x1b0404cb)]
    #[case(0x1903a30c)]
    #[case(0x18009645)]
    #[case(0x2000ffff)]
    #[case(0x01010000)]
    fn a_target_round_trips_through_its_compact_form(#[case] n_bits: u32) {
        let target = target_from_bits(n_bits).unwrap();

        assert_eq!(bits_from_target(target), n_bits);
    }

    #[test]
    fn a_target_encodes_to_one_form_however_it_was_written() {
        // Both name a target of exactly 1; only the second is minimal.
        assert_eq!(target_from_bits(0x03000001).unwrap(), U256::one());

        assert_eq!(bits_from_target(U256::one()), 0x01010000);
    }

    #[test]
    fn encoding_a_target_never_makes_it_easier_than_it_was() {
        for bits in [0x1d00fffe, 0x1c0abcde, 0x1e123456, 0x05012345] {
            let target = target_from_bits(bits).unwrap();
            let round_tripped = target_from_bits(bits_from_target(target)).unwrap();

            assert!(round_tripped <= target, "{bits:#010x} grew when encoded");
        }
    }

    #[test]
    fn a_target_of_zero_encodes_to_a_target_of_zero() {
        assert_eq!(
            target_from_bits(bits_from_target(U256::zero())).unwrap(),
            U256::zero()
        );
    }

    #[test]
    fn the_largest_target_that_fits_is_accepted_and_the_next_is_not() {
        assert!(target_from_bits(0x2100ffff).is_ok());
        assert!(target_from_bits(0x2200ffff).is_err());
    }

    #[rstest]
    #[case::sign_bit(0x1d80ffff)]
    #[case::sign_bit_and_nothing_else(0x00800000)]
    #[case::far_past_256_bits(0xff00ffff)]
    fn an_n_bits_that_names_no_target_is_refused(#[case] n_bits: u32) {
        assert!(target_from_bits(n_bits).is_err());
    }

    #[test]
    fn a_zero_mantissa_is_a_target_no_hash_can_be_under() {
        assert_eq!(target_from_bits(0x1d000000).unwrap(), U256::zero());
    }

    #[test]
    fn the_exponent_is_three_smaller_than_the_bytes_the_mantissa_occupies() {
        // The defect this replaces: 0x1d00ffff came out 2^24 too large, so a
        // header claiming Bitcoin's mainnet difficulty was 16 million times
        // cheaper to satisfy than it says.
        let correct = target_from_bits(0x1d00ffff).unwrap();

        assert_eq!(correct, U256::from(0xffffu32) << 208);
    }

    #[test]
    fn the_root_of_a_real_blocks_transactions_matches_its_published_root() {
        // Bitcoin block 170, the first payment ever made. Two transactions.
        let coinbase = leaf("b1fea52486ce0c62bb442b530a3f0132b826c74e473d1f2c220bfa78111c5082");
        let payment = leaf("f4184fc596403b9d638783cf57adfe4c75c605f6356fbc91338530e9831e9e16");

        assert_eq!(
            "7dac2c5666815c17a3b36427de37bb9d2e2c5ccec3f8633eb91a4205cb4c10ff",
            displayed(merkle_root(&[coinbase, payment]).unwrap())
        );
    }

    #[test]
    fn a_single_transaction_is_its_own_root() {
        // Bitcoin's genesis block.
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
        // Six is the smallest count where the two differ; they agree at 3 and 5.
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

    // A block's transactions must differ: two with one wtxid make it invalid.
    fn a_transaction(marker: u64) -> Transaction {
        let mut transaction = get_tx();
        transaction.outputs[0].value = Amount::from_atoms(10_000 + marker).unwrap();
        transaction
    }

    #[test]
    fn a_blocks_leaves_are_its_wtxids_in_order() {
        let first = a_transaction(1);
        let second = a_transaction(2);
        let mut block = get_block(0);
        block.transactions = vec![first.clone(), second.clone()];

        assert_eq!(
            node(
                *first.get_wtxid().as_bytes(),
                *second.get_wtxid().as_bytes()
            ),
            block.get_merkle_root_hash().unwrap(),
            "leaves are the wtxids, in order, and not byte-reversed"
        );
    }

    #[test]
    fn changing_only_a_witness_changes_the_root() {
        let mut block = get_block(2);
        let before = block.get_merkle_root_hash().unwrap();
        block.transactions[0].inputs[0].witness = Witness::new(vec![vec![0xfe; 64]]);

        assert_ne!(
            before,
            block.get_merkle_root_hash().unwrap(),
            "a root over txids would not have moved, and would commit no witness"
        );
    }

    #[test]
    fn a_block_holding_one_transaction_twice_has_no_root() {
        let mut block = get_block(2);
        block.transactions[1] = block.transactions[0].clone();

        assert!(
            block.get_merkle_root_hash().is_err(),
            "duplicate-last pairing is not injective, so the duplicate is what is refused"
        );
    }

    #[test]
    fn two_transactions_differing_only_in_witness_are_not_duplicates() {
        let mut block = get_block(2);
        block.transactions[1] = block.transactions[0].clone();
        block.transactions[1].inputs[0].witness = Witness::new(vec![vec![0xfe; 64]]);

        assert!(block.get_merkle_root_hash().is_ok());
    }

    fn a_block_of_one_transaction_serializing_to(size: usize) -> Block {
        let mut block = get_block(1);
        block.transactions[0] = Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data: Vec::new(),
                witness: Witness::empty(),
            }],
            outputs: vec![TxOut {
                value: Amount::from_atoms(1).unwrap(),
                script_pubkey: Vec::new(),
            }],
        };

        let short_by = size - block.transactions[0].get_raw_format().len();
        block.transactions[0].outputs[0].script_pubkey = vec![0; short_by];

        assert_eq!(block.transactions[0].get_raw_format().len(), size);
        block
    }

    #[rstest]
    #[case(MERKLE_NODE_SIZE - 1, true)]
    #[case(MERKLE_NODE_SIZE, false)]
    #[case(MERKLE_NODE_SIZE + 1, true)]
    fn only_a_transaction_the_size_of_a_merkle_node_costs_its_block_a_root(
        #[case] size: usize,
        #[case] has_a_root: bool,
    ) {
        let block = a_block_of_one_transaction_serializing_to(size);

        assert_eq!(block.get_merkle_root_hash().is_ok(), has_a_root);
    }

    #[test]
    fn a_block_with_no_transactions_has_no_root_rather_than_zeroes() {
        let block = get_block(0);

        assert!(
            block.get_merkle_root_hash().is_err(),
            "an all-zero root is a value a caller can mistake for a real one"
        );
    }

    /// The fixture carries Bitcoin's own proof-of-work limit, because the
    /// genesis known-answer test reproduces a real header. Nothing can search
    /// against that in a test, so anything that mines says so and uses this —
    /// an exponent above the real limit, so it could not appear in a header
    /// anyone else would accept.
    const SEARCHABLE_N_BITS: u32 = 0x2000ffff;

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
        block.n_bits = SEARCHABLE_N_BITS;

        // Asserts only that a nonce was found; any root satisfies that.
        assert!(block.mine().unwrap());
    }

    #[test]
    fn block_generates_correct_hash() {
        let mut block = get_block(2);

        block.prepare_for_mining().unwrap();

        let hash = get_hash(block.mine_array.as_slice());

        let hash = U256::from_little_endian(&hash);
        let target = target_from_bits(block.n_bits).unwrap();

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
        original_block.n_bits = SEARCHABLE_N_BITS;

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
            inputs: vec![TxIn {
                previous_output: Outpoint {
                    txid: Txid::from_bytes([0; 32]),
                    v_out: 0,
                },
                coinbase_data: Vec::new(),
                witness: Witness::new(vec![vec![7; 64], vec![8; 33]]),
            }],
            outputs: vec![TxOut {
                value: Amount::from_atoms(10_000).unwrap(),
                script_pubkey: vec![0x76, 0xa9],
            }],
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
            transactions: (0..number_of_transactions as u64)
                .map(a_transaction)
                .collect(),
        }
    }
}
