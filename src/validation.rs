use crate::amount::Amount;
use crate::params::Network;
use crate::script;
use crate::transaction::Transaction;
use crate::utxo::UtxoSet;
use anyhow::{anyhow, bail, Result};
use std::collections::HashSet;

pub const TRANSACTION_VERSION: u32 = 1;

/// ADR-0008: a height and room for an extranonce, and no more than Bitcoin
/// allows. The height must *match* the block's, which needs a block — that
/// half is M4's.
pub const MIN_COINBASE_DATA: usize = 4;
pub const MAX_COINBASE_DATA: usize = 100;

/// Everything a transaction can be judged on without looking at anything else.
/// A coinbase is exempt from the input rules by construction — its one input
/// points at no previous output — but not from the rest.
pub fn check_shape(transaction: &Transaction) -> Result<()> {
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
        .ok_or_else(|| anyhow!("the outputs sum past MAX_MONEY"))?;

    Ok(())
}

/// The rules that need the set of coins that exist. Returns the fee, which is
/// what a miner sorts by and what proves the sums were checked.
pub fn check_spend(
    transaction: &Transaction,
    utxo: &UtxoSet,
    spend_height: u32,
    network: Network,
) -> Result<Amount> {
    check_shape(transaction)?;

    if transaction.is_coinbase() {
        bail!("a coinbase is created by a block, not spent into one");
    }

    let txid = transaction.get_tx_id();
    let mut spent = Vec::new();

    for input in &transaction.inputs {
        let outpoint = input.previous_output;
        let coin = utxo
            .get(&outpoint)
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
    let paid_out = Amount::sum(transaction.outputs.iter().map(|output| output.value))
        .ok_or_else(|| anyhow!("the outputs sum past MAX_MONEY"))?;

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
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::crypto::PrivateKey;
    use crate::params::{MAINNET, TESTNET};
    use crate::transaction::{Outpoint, TxIn, TxOut, Witness};
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
        let (set, key, outpoint) = spendable();
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
