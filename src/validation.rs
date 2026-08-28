use crate::amount::{subsidy, Amount};
use crate::block::{Block, SharedHash};
use crate::blockchain::BlockIndex;
use crate::difficulty::{too_far_ahead, MAX_FUTURE_DRIFT};
use crate::params::Network;
use crate::script;
use crate::transaction::Transaction;
use crate::utxo::{Coins, UtxoView};
use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;

pub const TRANSACTION_VERSION: u32 = 1;

/// ADR-0008: a height and room for an extranonce, and no more than Bitcoin
/// allows. The height must *match* the block's, which needs a block — that
/// half is M4's.
pub const MIN_COINBASE_DATA: usize = 4;
pub const MAX_COINBASE_DATA: usize = 100;

/// ADR-0020. Bounds the work one message can demand: every input costs a
/// signature verification, and at 38 bytes each a 32 MiB payload would name
/// close to a million of them.
pub const MAX_TRANSACTION_SIZE: usize = 100_000;

/// A block has to fit in a message, and `MAX_PAYLOAD_SIZE` is 32 MiB. One
/// megabyte is Bitcoin's figure and is ten times what a thirty-second block
/// at demo volumes will ever hold — the point is that the number exists.
pub const MAX_BLOCK_SIZE: usize = 1_000_000;

/// Everything a transaction can be judged on without looking at anything else.
/// A coinbase is exempt from the input rules by construction — its one input
/// points at no previous output — but not from the rest.
///
/// Returns what the outputs sum to, because every caller needs it next and
/// summing again would be the same checked arithmetic twice.
pub fn check_shape(transaction: &Transaction) -> Result<Amount> {
    let size = transaction.get_raw_format().len();
    if size > MAX_TRANSACTION_SIZE {
        bail!("a transaction of {size} bytes is over {MAX_TRANSACTION_SIZE}");
    }

    if transaction.version != TRANSACTION_VERSION {
        bail!(
            "version {} is not {TRANSACTION_VERSION}",
            transaction.version
        );
    }
    if transaction.inputs.is_empty() {
        bail!("a transaction spends at least one input");
    }
    if transaction.outputs.is_empty() {
        bail!("a transaction pays at least one output");
    }

    let coinbase = transaction.is_coinbase();
    let mut seen = HashSet::new();
    for input in &transaction.inputs {
        if coinbase {
            let claimed = input.coinbase_data.len();
            if !(MIN_COINBASE_DATA..=MAX_COINBASE_DATA).contains(&claimed) {
                bail!(
                    "coinbase_data is {claimed} bytes, outside \
                     {MIN_COINBASE_DATA}..={MAX_COINBASE_DATA}"
                );
            }
        } else {
            if !input.coinbase_data.is_empty() {
                bail!("coinbase_data is empty on every input but a coinbase's");
            }
            if !seen.insert(input.previous_output) {
                bail!("{:?} is spent twice over", input.previous_output);
            }
        }
    }

    Amount::sum(transaction.outputs.iter().map(|output| output.value))
        .ok_or_else(|| anyhow!("the outputs sum past MAX_MONEY"))
}

/// A block refused because its timestamp is past the future limit. Told apart
/// from every other refusal because a node whose own clock is wrong rejects
/// what the network accepts, and that reads as a partition — ADR-0009 asks for
/// it to be logged loudly, and this is what a caller matches on.
#[derive(Debug)]
pub struct ClockDrift {
    pub timestamp: u32,
    pub now: u32,
}

impl std::fmt::Display for ClockDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a block claims {}, more than {MAX_FUTURE_DRIFT}s past this node's clock ({}); \
             check this machine's time before suspecting the network",
            self.timestamp, self.now
        )
    }
}

impl std::error::Error for ClockDrift {}

/// Everything a block must satisfy, in the order that refuses the cheap way
/// first. Returns the fees its transactions paid, which is what the coinbase
/// was allowed to claim and what a miner earns.
///
/// Reads no ambient state: the UTXO set, the block index, and the two values
/// passed in are all of it. That is what makes it safe to re-run during a
/// reorg (ADR-0012) — `now` only ever moves forward, so a block that passed
/// the future limit once still passes it.
pub fn check_block(
    block: &Block,
    index: &BlockIndex,
    utxo: &impl Coins,
    now: u32,
    network: Network,
) -> Result<Amount> {
    let height = check_header(block, index, now, network)?;

    check_body(block, height, utxo, network)
}

/// Everything a block can be judged on with the **index** and nothing else:
/// its work, the target the rule requires, its timestamp, its size and its
/// merkle root. Cheap, and none of it needs a coin.
///
/// Split from the rest so a caller can do this under the node lock and the
/// signatures without it — ADR-0020's shape, applied to a block.
pub fn check_header(block: &Block, index: &BlockIndex, now: u32, network: Network) -> Result<u32> {
    let header = block.header()?;
    let parent_hash = header.previous_block_hash;
    let parent = index
        .get(&parent_hash)
        .ok_or_else(|| anyhow!("{parent_hash} is not a block this node knows"))?;
    let height = parent.height + 1;

    let required = index.required_bits_after(&parent_hash, network)?;
    if header.n_bits != required {
        bail!(
            "block states n_bits {:#010x} where the rule requires {required:#010x}",
            header.n_bits
        );
    }
    if !header.meets_its_target()? {
        bail!("block {} does not meet its own target", header.hash());
    }

    if too_far_ahead(header.time, now) {
        return Err(ClockDrift {
            timestamp: header.time,
            now,
        }
        .into());
    }
    let median = index.median_time_after(&parent_hash)?;
    if header.time <= median {
        bail!(
            "block claims {}, not past the median of the last eleven, {median}",
            header.time
        );
    }

    let size = block.get_raw_format()?.len();
    if size > MAX_BLOCK_SIZE {
        bail!("a block of {size} bytes is over {MAX_BLOCK_SIZE}");
    }

    // Recomputing the root is also what enforces no duplicate wtxid and no
    // 64-byte transaction: a block carrying either has no root at all.
    // A `SharedHash`, because a block's hash commits to its header and the
    // header commits to a root this body does not match. Some other body does,
    // and refusing this one must not refuse that one.
    if block.get_merkle_root_hash()? != header.merkle_root {
        return Err(SharedHash("the merkle root does not cover these transactions".into()).into());
    }

    Ok(height)
}

/// The expensive half: a signature and a script per input, and the fee
/// arithmetic that depends on them. Needs no index — `height` is what the
/// header check worked out — so it can run with nothing held.
pub fn check_body(
    block: &Block,
    height: u32,
    utxo: &impl Coins,
    network: Network,
) -> Result<Amount> {
    let (coinbase, rest) = block
        .transactions
        .split_first()
        .ok_or_else(|| anyhow!("a block has at least a coinbase"))?;
    if !coinbase.is_coinbase() {
        bail!("a block's first transaction is its coinbase");
    }
    if let Some(position) = rest.iter().position(Transaction::is_coinbase) {
        bail!("transaction {} is a second coinbase", position + 1);
    }

    let mut view = UtxoView::over(utxo);
    view.apply(coinbase, height)?;

    let mut fees = Amount::ZERO;
    for transaction in rest {
        let fee = check_spend(transaction, &view, height, network)?;
        fees = fees
            .checked_add(fee)
            .ok_or_else(|| anyhow!("the fees sum past MAX_MONEY"))?;
        view.apply(transaction, height)?;
    }

    check_coinbase(coinbase, height, fees)?;

    Ok(fees)
}

/// What a block's first transaction must be.
///
/// `fees` is what the rest of the block paid, since a coinbase may claim it
/// (ADR-0008) — so a caller must derive it from `check_spend`, never from
/// anything the block says about itself.
pub fn check_coinbase(transaction: &Transaction, height: u32, fees: Amount) -> Result<()> {
    let claimed = check_shape(transaction)?;

    if !transaction.is_coinbase() {
        bail!("a block's first transaction is a coinbase");
    }

    let stated = transaction.inputs[0]
        .coinbase_data
        .first_chunk::<4>()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .expect("check_shape refuses coinbase_data under four bytes");
    if stated != height {
        bail!("coinbase_data opens with height {stated}, in a block at {height}");
    }

    let allowed = subsidy(height)
        .checked_add(fees)
        .ok_or_else(|| anyhow!("the subsidy and fees sum past MAX_MONEY"))?;

    if claimed > allowed {
        bail!("the coinbase claims {claimed} where {allowed} is owed");
    }

    Ok(())
}

/// The rules that need the set of coins that exist. Returns the fee, which is
/// what a miner sorts by and what proves the sums were checked.
pub fn check_spend(
    transaction: &Transaction,
    coins: &impl Coins,
    spend_height: u32,
    network: Network,
) -> Result<Amount> {
    let paid_out = check_shape(transaction)?;

    if transaction.is_coinbase() {
        bail!("a coinbase is created by a block, not spent into one");
    }

    let txid = transaction.get_tx_id();
    let mut spent = Vec::new();

    for input in &transaction.inputs {
        let outpoint = input.previous_output;
        let coin = coins
            .coin(&outpoint)
            .ok_or_else(|| anyhow!("{outpoint:?} is not an unspent output"))?;

        if !coin.spendable_at(spend_height, network.maturity) {
            bail!(
                "{outpoint:?} is a coinbase from height {} and is not yet {} blocks deep",
                coin.height,
                network.maturity
            );
        }

        script::execute(&coin.output.script_pubkey, &input.witness, txid)
            .map_err(|why| anyhow!("{outpoint:?} does not unlock: {why}"))?;

        spent.push(coin.output.value);
    }

    let paid_in = Amount::sum(spent).ok_or_else(|| anyhow!("the inputs sum past MAX_MONEY"))?;

    paid_in
        .checked_sub(paid_out)
        .ok_or_else(|| anyhow!("pays out {paid_out} against {paid_in} in"))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use crate::amount::Amount;
    use crate::crypto::{PrivateKey, PubKeyHash};
    use crate::script::p2pkh;
    use crate::transaction::{Outpoint, Transaction, TxIn, TxOut, Witness};
    use crate::util::hash160;
    use crate::utxo::UtxoSet;

    pub fn pay_to(key: &PrivateKey, atoms: u64) -> TxOut {
        TxOut {
            value: Amount::from_atoms(atoms).unwrap(),
            script_pubkey: p2pkh(&PubKeyHash::from_bytes(hash160(
                key.public_key().as_bytes(),
            ))),
        }
    }

    /// A coinbase paying `key`, connected at `height`, and the outpoint of its
    /// first output — the only way a coin comes into existence.
    pub fn funded(set: &mut UtxoSet, key: &PrivateKey, atoms: u64, height: u32) -> Outpoint {
        let coinbase = Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data: [height.to_le_bytes(), (atoms as u32).to_le_bytes()].concat(),
                witness: Witness::empty(),
            }],
            outputs: vec![pay_to(key, atoms)],
        };
        set.connect(&coinbase, height).unwrap();

        Outpoint {
            txid: coinbase.get_tx_id(),
            v_out: 0,
        }
    }

    /// Signs every input with the one digest ADR-0004 fixed: the txid, which
    /// the witnesses are not part of, so adding them cannot move it.
    pub fn signed(key: &PrivateKey, spending: &[Outpoint], outputs: Vec<TxOut>) -> Transaction {
        let mut transaction = Transaction {
            version: 1,
            inputs: spending
                .iter()
                .map(|previous_output| TxIn {
                    previous_output: *previous_output,
                    coinbase_data: Vec::new(),
                    witness: Witness::empty(),
                })
                .collect(),
            outputs,
        };

        let txid = transaction.get_tx_id();
        for input in &mut transaction.inputs {
            input.witness = Witness::new(vec![
                key.sign(txid.as_bytes()).as_bytes().to_vec(),
                key.public_key().as_bytes().to_vec(),
            ]);
        }

        transaction
    }
}

#[cfg(test)]
mod block_tests {
    use super::fixtures::*;
    use super::*;
    use crate::block::Block;
    use crate::crypto::PrivateKey;
    use crate::params::TESTNET;
    use crate::utxo::UtxoSet;

    const TARGET_BLOCK_TIME: u32 = TESTNET.target_block_time;

    fn a_chain() -> (BlockIndex, UtxoSet, u32) {
        let genesis = TESTNET.genesis().unwrap();
        let mut utxo = UtxoSet::new();
        utxo.connect(&genesis.transactions[0], 0).unwrap();

        let index = BlockIndex::new(genesis.header().unwrap()).unwrap();
        let now = genesis.time + 10 * TARGET_BLOCK_TIME;

        (index, utxo, now)
    }

    /// A block that satisfies every rule, mined at the test network's
    /// deliberately trivial difficulty. `tweak` runs *before* mining, so a
    /// test that breaks one rule does not accidentally break proof-of-work too.
    fn mined(
        index: &BlockIndex,
        utxo: &UtxoSet,
        payments: Vec<Transaction>,
        tweak: impl FnOnce(&mut Block),
    ) -> Block {
        let parent = index.best();
        let height = parent.height + 1;

        let mut view = UtxoView::over(utxo);
        let mut fees = Amount::ZERO;
        for payment in &payments {
            fees = fees
                .checked_add(check_spend(payment, &view, height, &TESTNET).unwrap())
                .unwrap();
            view.apply(payment, height).unwrap();
        }

        let owed = subsidy(height).checked_add(fees).unwrap();
        let coinbase =
            Transaction::coinbase(height, 0, vec![pay_to(&PrivateKey::random(), owed.atoms())]);

        let mut block = Block::new(
            1,
            *parent.header.hash().as_bytes(),
            parent.header.time + TARGET_BLOCK_TIME,
            index
                .required_bits_after(&index.best_hash(), &TESTNET)
                .unwrap(),
            [vec![coinbase], payments].concat(),
        );

        tweak(&mut block);
        assert!(block.mine().unwrap(), "the test network mines in a moment");

        block
    }

    fn valid(index: &BlockIndex, utxo: &UtxoSet) -> Block {
        mined(index, utxo, Vec::new(), |_| {})
    }

    #[test]
    fn a_block_that_breaks_no_rule_is_accepted() {
        let (index, utxo, now) = a_chain();

        let fees = check_block(&valid(&index, &utxo), &index, &utxo, now, &TESTNET).unwrap();

        assert_eq!(fees, Amount::ZERO, "a block of one coinbase earns nothing");
    }

    #[test]
    fn validating_the_same_block_twice_gives_the_same_answer() {
        let (index, utxo, now) = a_chain();
        let block = valid(&index, &utxo);

        let first = check_block(&block, &index, &utxo, now, &TESTNET).unwrap();
        let again = check_block(&block, &index, &utxo, now, &TESTNET).unwrap();

        assert_eq!(
            first, again,
            "nothing outside the set and the index is read"
        );
    }

    #[test]
    fn a_block_whose_parent_is_unknown_is_refused() {
        let (index, utxo, now) = a_chain();
        let mut block = valid(&index, &utxo);
        block.previous_block_hash = [9; 32];

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("not a block this node knows"), "{refusal}");
    }

    #[test]
    fn a_block_whose_work_does_not_meet_its_target_is_refused() {
        let (index, utxo, now) = a_chain();
        let mut block = valid(&index, &utxo);
        while block.header().unwrap().meets_its_target().unwrap() {
            block.nonce = block.nonce.wrapping_add(1);
        }

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("target"), "{refusal}");
    }

    #[test]
    fn a_block_stating_an_easier_target_than_the_rule_requires_is_refused() {
        let (index, utxo, now) = a_chain();
        let block = mined(&index, &utxo, Vec::new(), |block| {
            block.n_bits = 0x2100ffff;
        });

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("n_bits"), "{refusal}");
    }

    #[test]
    fn a_block_from_the_future_is_refused_in_a_way_a_caller_can_single_out() {
        let (index, utxo, now) = a_chain();
        let block = mined(&index, &utxo, Vec::new(), |block| {
            block.time = now + crate::difficulty::MAX_FUTURE_DRIFT + 1;
        });

        let refusal = check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err();

        assert!(
            refusal.downcast_ref::<ClockDrift>().is_some(),
            "the loud rejection has to be distinguishable: {refusal:#}"
        );
    }

    #[test]
    fn a_block_not_past_the_median_of_the_last_eleven_is_refused() {
        let (index, utxo, now) = a_chain();
        let median = index.median_time_after(&index.best_hash()).unwrap();
        let block = mined(&index, &utxo, Vec::new(), |block| {
            block.time = median;
        });

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("median"), "{refusal}");
        assert!(
            check_block(&block, &index, &utxo, now, &TESTNET)
                .unwrap_err()
                .downcast_ref::<ClockDrift>()
                .is_none(),
            "an ordinary refusal, not the one that blames the clock"
        );
    }

    #[test]
    fn a_block_whose_merkle_root_does_not_cover_its_transactions_is_refused() {
        let (index, utxo, now) = a_chain();
        let mut block = valid(&index, &utxo);
        // Swapping the body rather than the root: the root is what was mined,
        // so a block with a bogus root fails proof-of-work first and the test
        // would be about the wrong rule.
        block.transactions = vec![Transaction::coinbase(
            1,
            0xabcd,
            vec![pay_to(&PrivateKey::random(), subsidy(1).atoms())],
        )];

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("merkle"), "{refusal}");
    }

    #[test]
    fn a_block_with_no_coinbase_first_is_refused() {
        let (index, utxo, now) = a_chain();
        let key = PrivateKey::random();
        let mut seeded = utxo;
        let outpoint = funded(&mut seeded, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);

        let block = mined(&index, &seeded, vec![payment], |block| {
            block.transactions.remove(0);
        });

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &seeded, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("first transaction"), "{refusal}");
    }

    #[test]
    fn a_block_whose_coinbase_is_not_first_is_refused() {
        let (index, mut utxo, now) = a_chain();
        let key = PrivateKey::random();
        let outpoint = funded(&mut utxo, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);

        let block = mined(&index, &utxo, vec![payment], |block| {
            block.transactions.swap(0, 1);
        });

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("first transaction"), "{refusal}");
    }

    #[test]
    fn a_block_with_a_second_coinbase_is_refused() {
        let (index, utxo, now) = a_chain();
        let block = mined(&index, &utxo, Vec::new(), |block| {
            let another = Transaction::coinbase(1, 99, vec![pay_to(&PrivateKey::random(), 1)]);
            block.transactions.push(another);
        });

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("second coinbase"), "{refusal}");
    }

    #[test]
    fn a_block_whose_two_transactions_spend_the_same_output_is_refused() {
        let (index, mut utxo, now) = a_chain();
        let key = PrivateKey::random();
        let outpoint = funded(&mut utxo, &key, 1_000, 0);
        let first = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let second = signed(&key, &[outpoint], vec![pay_to(&key, 800)]);

        let block = mined(&index, &utxo, vec![first], |block| {
            block.transactions.push(second);
        });

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("not an unspent output"), "{refusal}");
    }

    #[test]
    fn a_block_paying_its_miner_more_than_it_earned_is_refused() {
        let (index, utxo, now) = a_chain();
        let block = mined(&index, &utxo, Vec::new(), |block| {
            block.transactions[0].outputs[0].value =
                Amount::from_atoms(subsidy(1).atoms() + 1).unwrap();
        });

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("claims"), "{refusal}");
    }

    #[test]
    fn a_block_paying_its_miner_less_than_it_earned_is_accepted() {
        let (index, utxo, now) = a_chain();
        let block = mined(&index, &utxo, Vec::new(), |block| {
            block.transactions[0].outputs[0].value = Amount::from_atoms(1).unwrap();
        });

        assert!(check_block(&block, &index, &utxo, now, &TESTNET).is_ok());
    }

    #[test]
    fn a_blocks_fees_are_what_its_miner_may_claim() {
        let (index, mut utxo, now) = a_chain();
        let key = PrivateKey::random();
        let outpoint = funded(&mut utxo, &key, 1_000, 0);
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);

        let block = mined(&index, &utxo, vec![payment], |_| {});
        let fees = check_block(&block, &index, &utxo, now, &TESTNET).unwrap();

        assert_eq!(fees, Amount::from_atoms(100).unwrap());
        assert_eq!(
            block.transactions[0].outputs[0].value,
            subsidy(1).checked_add(fees).unwrap()
        );
    }

    #[test]
    fn a_transaction_may_spend_what_an_earlier_one_in_the_same_block_created() {
        let (index, mut utxo, now) = a_chain();
        let key = PrivateKey::random();
        let outpoint = funded(&mut utxo, &key, 1_000, 0);
        let first = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let chained = signed(
            &key,
            &[crate::transaction::Outpoint {
                txid: first.get_tx_id(),
                v_out: 0,
            }],
            vec![pay_to(&key, 850)],
        );

        let block = mined(&index, &utxo, vec![first, chained], |_| {});

        assert_eq!(
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap(),
            Amount::from_atoms(150).unwrap()
        );
    }

    #[test]
    fn a_block_whose_payment_does_not_validate_is_refused() {
        let (index, mut utxo, now) = a_chain();
        let key = PrivateKey::random();
        let outpoint = funded(&mut utxo, &key, 1_000, 0);
        let stranger = signed(&PrivateKey::random(), &[outpoint], vec![pay_to(&key, 900)]);

        let block = mined(&index, &utxo, Vec::new(), |block| {
            block.transactions.push(stranger);
        });

        let refusal = format!(
            "{:#}",
            check_block(&block, &index, &utxo, now, &TESTNET).unwrap_err()
        );

        assert!(refusal.contains("does not unlock"), "{refusal}");
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::crypto::PrivateKey;
    use crate::params::{MAINNET, TESTNET};
    use crate::transaction::{Outpoint, TxIn, TxOut, Witness};
    use crate::utxo::UtxoSet;
    use rstest::rstest;

    const AFTER_MATURITY: u32 = 500;

    fn spendable() -> (UtxoSet, PrivateKey, Outpoint) {
        let mut set = UtxoSet::new();
        let key = PrivateKey::random();
        let outpoint = funded(&mut set, &key, 1_000, 0);

        (set, key, outpoint)
    }

    #[test]
    fn a_signed_spend_of_a_mature_coin_is_valid_and_its_fee_is_the_difference() {
        let (set, key, outpoint) = spendable();
        let transaction = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);

        let fee = check_spend(&transaction, &set, AFTER_MATURITY, &MAINNET).unwrap();

        assert_eq!(fee, Amount::from_atoms(100).unwrap());
    }

    #[test]
    fn a_spend_of_an_output_that_does_not_exist_is_refused() {
        let (set, key, _) = spendable();
        let invented = Outpoint {
            txid: crate::transaction::Txid::from_bytes([9; 32]),
            v_out: 0,
        };

        let transaction = signed(&key, &[invented], vec![pay_to(&key, 1)]);

        assert!(check_spend(&transaction, &set, AFTER_MATURITY, &MAINNET).is_err());
    }

    #[test]
    fn a_spend_of_the_same_outpoint_twice_is_refused() {
        let (set, key, outpoint) = spendable();
        let transaction = signed(&key, &[outpoint, outpoint], vec![pay_to(&key, 900)]);

        assert!(check_spend(&transaction, &set, AFTER_MATURITY, &MAINNET).is_err());
    }

    #[test]
    fn paying_out_more_than_was_paid_in_is_refused() {
        let (set, key, outpoint) = spendable();
        let transaction = signed(&key, &[outpoint], vec![pay_to(&key, 1_001)]);

        assert!(check_spend(&transaction, &set, AFTER_MATURITY, &MAINNET).is_err());
    }

    #[test]
    fn paying_out_exactly_what_was_paid_in_is_a_zero_fee_and_legal() {
        let (set, key, outpoint) = spendable();
        let transaction = signed(&key, &[outpoint], vec![pay_to(&key, 1_000)]);

        let fee = check_spend(&transaction, &set, AFTER_MATURITY, &MAINNET).unwrap();

        assert_eq!(fee, Amount::ZERO);
    }

    #[test]
    fn a_witness_that_does_not_satisfy_the_script_is_refused() {
        let (set, key, outpoint) = spendable();
        let stranger = PrivateKey::random();
        let transaction = signed(&stranger, &[outpoint], vec![pay_to(&key, 900)]);

        assert!(check_spend(&transaction, &set, AFTER_MATURITY, &MAINNET).is_err());
    }

    #[test]
    fn a_signature_over_a_different_transaction_does_not_carry_over() {
        let (set, key, outpoint) = spendable();
        let honest = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let mut redirected = signed(&key, &[outpoint], vec![pay_to(&PrivateKey::random(), 900)]);
        redirected.inputs[0].witness = honest.inputs[0].witness.clone();

        assert!(check_spend(&redirected, &set, AFTER_MATURITY, &MAINNET).is_err());
    }

    #[test]
    fn an_immature_coinbase_output_cannot_be_spent_and_a_mature_one_can() {
        let (set, key, outpoint) = spendable();
        let transaction = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);

        assert!(check_spend(&transaction, &set, MAINNET.maturity - 1, &MAINNET).is_err());
        assert!(check_spend(&transaction, &set, MAINNET.maturity, &MAINNET).is_ok());
    }

    #[test]
    fn the_test_network_maturity_is_the_one_that_applies_on_it() {
        let (set, key, outpoint) = spendable();
        let transaction = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);

        assert!(check_spend(&transaction, &set, TESTNET.maturity, &TESTNET).is_ok());
    }

    #[test]
    fn a_version_other_than_one_is_refused() {
        let (set, key, outpoint) = spendable();
        let mut transaction = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        transaction.version = 2;

        assert!(check_spend(&transaction, &set, AFTER_MATURITY, &MAINNET).is_err());
    }

    #[test]
    fn coinbase_data_on_an_ordinary_input_is_refused() {
        let (set, key, outpoint) = spendable();
        let mut transaction = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        transaction.inputs[0].coinbase_data = vec![1];

        assert!(check_spend(&transaction, &set, AFTER_MATURITY, &MAINNET).is_err());
    }

    /// Against `check_shape`, not `check_spend`: with no inputs the balance
    /// check would refuse this anyway, and a test that cannot tell which rule
    /// caught it is not a test of either.
    #[test]
    fn a_transaction_that_spends_nothing_is_refused_for_that() {
        let (_set, key, outpoint) = spendable();
        let mut no_inputs = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        no_inputs.inputs.clear();

        let refusal = format!("{:#}", check_shape(&no_inputs).unwrap_err());

        assert!(refusal.contains("at least one input"), "{refusal}");
    }

    #[test]
    fn a_transaction_that_pays_nobody_is_refused_for_that() {
        let (_set, key, outpoint) = spendable();
        let mut no_outputs = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        no_outputs.outputs.clear();

        let refusal = format!("{:#}", check_shape(&no_outputs).unwrap_err());

        assert!(refusal.contains("at least one output"), "{refusal}");
    }

    #[test]
    fn outputs_summing_past_max_money_are_refused_before_any_coin_is_looked_up() {
        let (_set, key, outpoint) = spendable();
        let half = Amount::from_atoms(crate::amount::MAX_MONEY / 2 + 1).unwrap();
        let huge = TxOut {
            value: half,
            script_pubkey: pay_to(&key, 1).script_pubkey,
        };
        let transaction = signed(&key, &[outpoint], vec![huge.clone(), huge]);

        assert!(check_shape(&transaction).is_err());
    }

    #[rstest]
    #[case::empty(0, false)]
    #[case::under_the_height(MIN_COINBASE_DATA - 1, false)]
    #[case::just_the_height(MIN_COINBASE_DATA, true)]
    #[case::room_for_an_extranonce(50, true)]
    #[case::at_the_cap(MAX_COINBASE_DATA, true)]
    #[case::over_the_cap(MAX_COINBASE_DATA + 1, false)]
    fn a_coinbases_data_carries_a_height_and_no_more_than_bitcoin_allows(
        #[case] length: usize,
        #[case] legal: bool,
    ) {
        let key = PrivateKey::random();
        let coinbase = Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data: vec![0; length],
                witness: Witness::empty(),
            }],
            outputs: vec![pay_to(&key, 50)],
        };

        assert_eq!(check_shape(&coinbase).is_ok(), legal);
    }

    /// A coinbase's null outpoint is never in the set, so "not an unspent
    /// output" would refuse this too. The message is what says which rule ran.
    fn a_coinbase(height: u32, atoms: u64) -> Transaction {
        Transaction::coinbase(height, 0, vec![pay_to(&PrivateKey::random(), atoms)])
    }

    #[test]
    fn a_coinbase_may_claim_the_subsidy_and_the_fees_and_no_more() {
        let fees = Amount::from_atoms(700).unwrap();
        let owed = subsidy(1).atoms() + 700;

        assert!(check_coinbase(&a_coinbase(1, owed), 1, fees).is_ok());
        assert!(check_coinbase(&a_coinbase(1, owed + 1), 1, fees).is_err());
    }

    #[test]
    fn a_coinbase_claiming_less_than_it_is_owed_burns_the_difference() {
        assert!(check_coinbase(&a_coinbase(1, 1), 1, Amount::ZERO).is_ok());
    }

    #[test]
    fn a_coinbase_naming_another_blocks_height_is_refused() {
        let refusal = format!(
            "{:#}",
            check_coinbase(&a_coinbase(8, 10), 9, Amount::ZERO).unwrap_err()
        );

        assert!(refusal.contains("height 8"), "{refusal}");
    }

    #[test]
    fn an_ordinary_transaction_offered_as_a_coinbase_is_refused() {
        let (_set, key, outpoint) = spendable();
        let ordinary = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);

        assert!(check_coinbase(&ordinary, 1, Amount::ZERO).is_err());
    }

    #[test]
    fn a_coinbase_past_the_last_halving_may_claim_only_the_fees() {
        let height = 33 * crate::amount::HALVING_INTERVAL;
        let fees = Amount::from_atoms(500).unwrap();

        assert_eq!(subsidy(height), Amount::ZERO);
        assert!(check_coinbase(&a_coinbase(height, 500), height, fees).is_ok());
        assert!(check_coinbase(&a_coinbase(height, 501), height, fees).is_err());
    }

    #[test]
    fn a_built_coinbase_is_one_a_validator_accepts() {
        let built = Transaction::coinbase(
            7,
            0xdead_beef,
            vec![pay_to(&PrivateKey::random(), subsidy(7).atoms())],
        );

        assert!(check_coinbase(&built, 7, Amount::ZERO).is_ok());
    }

    #[test]
    fn a_transaction_too_large_to_be_worth_verifying_is_refused_first() {
        let (_set, key, outpoint) = spendable();
        let mut huge = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        huge.outputs[0].script_pubkey = vec![0; MAX_TRANSACTION_SIZE];

        let refusal = format!("{:#}", check_shape(&huge).unwrap_err());

        assert!(refusal.contains("over"), "{refusal}");
    }

    #[test]
    fn a_transaction_at_the_size_limit_is_not_refused_for_its_size() {
        let (_set, key, outpoint) = spendable();
        let mut at_limit = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        // The compact-size prefix widens as the script grows, so converge.
        while at_limit.get_raw_format().len() < MAX_TRANSACTION_SIZE {
            let spare = MAX_TRANSACTION_SIZE - at_limit.get_raw_format().len();
            at_limit.outputs[0].script_pubkey.extend(vec![0; spare]);
        }
        while at_limit.get_raw_format().len() > MAX_TRANSACTION_SIZE {
            at_limit.outputs[0].script_pubkey.pop();
        }

        assert_eq!(at_limit.get_raw_format().len(), MAX_TRANSACTION_SIZE);
        assert!(check_shape(&at_limit).is_ok());
    }

    #[test]
    fn a_coinbase_is_not_something_a_peer_spends_into_the_chain() {
        let set = UtxoSet::new();
        let key = PrivateKey::random();
        let coinbase = Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data: vec![0, 0, 0, 0],
                witness: Witness::empty(),
            }],
            outputs: vec![pay_to(&key, 50)],
        };

        assert!(check_shape(&coinbase).is_ok(), "it is a legal shape");
        let refusal = format!(
            "{:#}",
            check_spend(&coinbase, &set, AFTER_MATURITY, &MAINNET).unwrap_err()
        );

        assert!(refusal.contains("created by a block"), "{refusal}");
    }
}
