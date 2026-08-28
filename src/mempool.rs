use crate::amount::Amount;
use crate::params::Network;
use crate::transaction::{Outpoint, Transaction, Txid};
use crate::utxo::{Coin, UtxoSet};
use crate::validation::check_spend;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;

/// Bounded for the same reason `MAX_PEERS` and `OUTBOUND_QUEUE` are: a peer
/// relaying nothing but valid transactions must not be able to exhaust memory.
pub const MAX_MEMPOOL: usize = 5_000;

#[derive(Clone, Debug)]
pub struct Entry {
    pub transaction: Transaction,
    pub fee: Amount,
}

#[derive(Debug, Default)]
pub struct Mempool {
    entries: HashMap<Txid, Entry>,
    /// Which held transaction is spending each outpoint, so a conflict is a
    /// lookup rather than a scan.
    claimed: HashMap<Outpoint, Txid>,
}

impl Mempool {
    pub fn new() -> Self {
        Mempool::default()
    }

    /// Everything that can refuse a transaction without checking a signature:
    /// already held, past the bound, or spending what something else here
    /// already spends.
    ///
    /// Separate from `accept` because it is what runs under the node's lock
    /// while the expensive half does not — ADR-0020.
    pub fn admissible(&self, txid: Txid, transaction: &Transaction) -> Result<()> {
        if self.entries.contains_key(&txid) {
            bail!("{txid} is already held");
        }
        if self.entries.len() >= MAX_MEMPOOL {
            bail!("the mempool holds {MAX_MEMPOOL} transactions");
        }

        for input in &transaction.inputs {
            if let Some(holder) = self.claimed.get(&input.previous_output) {
                bail!("{:?} is already spent by {holder}", input.previous_output);
            }
        }

        Ok(())
    }

    /// Validates and holds, all under whatever lock the caller has. Used where
    /// the work is already bounded by a block's contents; a peer's
    /// transaction takes the two-phase path instead.
    pub fn accept(
        &mut self,
        transaction: Transaction,
        utxo: &UtxoSet,
        spend_height: u32,
        network: Network,
    ) -> Result<Txid> {
        let txid = transaction.get_tx_id();
        // Before validation, not after: a conflict is a hash lookup and
        // validation is a signature check per input, so the cheap refusal has
        // to come first or a peer can spend our CPU by conflicting with what
        // we already hold, over and over, for free.
        self.admissible(txid, &transaction)?;

        let fee = check_spend(&transaction, utxo, spend_height, network)?;
        self.hold(txid, transaction, fee);

        Ok(txid)
    }

    /// Holds a transaction whose signatures were checked without the lock,
    /// against coins looked up at the time.
    ///
    /// The set may have moved since — a block can connect, or a reorg
    /// disconnect one — so every coin it was validated against is confirmed to
    /// still be there, unchanged, and spendable **at the height the caller now
    /// reads**, not the one it read before. A reorg lowers the tip, and a
    /// stale higher height would let an immature coinbase through.
    ///
    /// `fee` is the one `check_spend` returned for these coins. Because every
    /// one of them is confirmed identical here, re-deriving it would give the
    /// same answer from the same numbers.
    pub fn admit(
        &mut self,
        transaction: Transaction,
        verified_against: &HashMap<Outpoint, Coin>,
        fee: Amount,
        utxo: &UtxoSet,
        spend_height: u32,
        network: Network,
    ) -> Result<Txid> {
        let txid = transaction.get_tx_id();
        self.admissible(txid, &transaction)?;

        for input in &transaction.inputs {
            let outpoint = input.previous_output;
            let coin = utxo
                .get(&outpoint)
                .ok_or_else(|| anyhow!("{outpoint:?} was spent while this was checked"))?;

            if verified_against.get(&outpoint) != Some(&coin) {
                bail!("{outpoint:?} is not the coin this was checked against");
            }
            if !coin.spendable_at(spend_height, network.maturity) {
                bail!("{outpoint:?} is not spendable at {spend_height}");
            }
        }

        self.hold(txid, transaction, fee);

        Ok(txid)
    }

    fn hold(&mut self, txid: Txid, transaction: Transaction, fee: Amount) {
        for input in &transaction.inputs {
            self.claimed.insert(input.previous_output, txid);
        }
        self.entries.insert(txid, Entry { transaction, fee });
    }

    pub fn contains(&self, txid: &Txid) -> bool {
        self.entries.contains_key(txid)
    }

    pub fn get(&self, txid: &Txid) -> Option<&Transaction> {
        self.entries.get(txid).map(|entry| &entry.transaction)
    }

    pub fn remove(&mut self, txid: &Txid) -> Option<Entry> {
        let entry = self.entries.remove(txid)?;
        for input in &entry.transaction.inputs {
            self.claimed.remove(&input.previous_output);
        }

        Some(entry)
    }

    /// What a miner would put in a block, most valuable first. Fee alone
    /// rather than fee per byte: transactions here are one shape and within a
    /// few bytes of each other, so the finer measure would be noise.
    /// The `at_most` richest, cloned — and only those. `by_fee` clones every
    /// transaction it holds, which is up to `MAX_MEMPOOL` × the transaction
    /// bound, and a caller that wanted twenty of them should not pay for that.
    pub fn richest(&self, at_most: usize) -> Vec<Entry> {
        let mut order: Vec<(&Txid, Amount)> = self
            .entries
            .iter()
            .map(|(txid, entry)| (txid, entry.fee))
            .collect();
        order.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(right.0)));

        order
            .into_iter()
            .take(at_most)
            .filter_map(|(txid, _)| self.entries.get(txid).cloned())
            .collect()
    }

    pub fn by_fee(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self.entries.values().cloned().collect();
        entries.sort_by(|left, right| {
            right.fee.cmp(&left.fee).then_with(|| {
                left.transaction
                    .get_tx_id()
                    .cmp(&right.transaction.get_tx_id())
            })
        });

        entries
    }

    pub fn txids(&self) -> Vec<Txid> {
        self.entries.keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::PrivateKey;
    use crate::params::MAINNET;
    use crate::transaction::{Outpoint, TxIn, Witness};
    use crate::validation::fixtures::{funded, pay_to, signed};

    const AFTER_MATURITY: u32 = 500;

    #[allow(clippy::type_complexity)]
    fn a_funded_wallet() -> (UtxoSet, PrivateKey, Vec<Outpoint>) {
        let mut set = UtxoSet::new();
        let key = PrivateKey::random();
        let outpoints = (0..3)
            .map(|n| funded(&mut set, &key, 1_000 + n, 0))
            .collect();

        (set, key, outpoints)
    }

    fn accept(pool: &mut Mempool, set: &UtxoSet, transaction: Transaction) -> Result<Txid> {
        pool.accept(transaction, set, AFTER_MATURITY, &MAINNET)
    }

    #[test]
    fn a_valid_transaction_is_held_and_findable_by_its_txid() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let transaction = signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]);

        let txid = accept(&mut pool, &set, transaction.clone()).unwrap();

        assert!(pool.contains(&txid));
        assert_eq!(pool.get(&txid), Some(&transaction));
        assert_eq!(pool.txids(), vec![txid]);
    }

    #[test]
    fn an_invalid_transaction_is_not_held() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let overspending = signed(&key, &outpoints[..1], vec![pay_to(&key, 9_000)]);

        assert!(accept(&mut pool, &set, overspending).is_err());
        assert!(pool.is_empty());
    }

    #[test]
    fn a_coinbase_is_refused_outright() {
        let (set, key, _) = a_funded_wallet();
        let mut pool = Mempool::new();
        // Its null outpoint is never in the set either, so the message is what
        // says which rule refused it.
        let coinbase = Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data: vec![0, 0, 0, 0],
                witness: Witness::empty(),
            }],
            outputs: vec![pay_to(&key, 50)],
        };

        let refusal = format!("{:#}", accept(&mut pool, &set, coinbase).unwrap_err());

        assert!(refusal.contains("created by a block"), "{refusal}");
    }

    #[test]
    fn a_second_transaction_spending_a_held_outpoint_is_refused() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        accept(
            &mut pool,
            &set,
            signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]),
        )
        .unwrap();

        let conflict = signed(&key, &outpoints[..1], vec![pay_to(&key, 800)]);

        assert!(accept(&mut pool, &set, conflict).is_err());
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn a_conflict_is_refused_without_the_signature_ever_being_checked() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        accept(
            &mut pool,
            &set,
            signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]),
        )
        .unwrap();

        let mut garbage = signed(&key, &outpoints[..1], vec![pay_to(&key, 800)]);
        garbage.inputs[0].witness = Witness::new(vec![vec![0; 64], vec![0; 33]]);

        let refusal = format!("{:#}", accept(&mut pool, &set, garbage).unwrap_err());

        assert!(
            refusal.contains("already spent by"),
            "the cheap check has to come first: {refusal}"
        );
    }

    #[test]
    fn the_same_transaction_offered_twice_is_held_once() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let transaction = signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]);

        accept(&mut pool, &set, transaction.clone()).unwrap();

        assert!(accept(&mut pool, &set, transaction).is_err());
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn removing_a_transaction_releases_the_outpoints_it_claimed() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let first = signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]);
        let txid = accept(&mut pool, &set, first).unwrap();

        pool.remove(&txid).unwrap();

        let second = signed(&key, &outpoints[..1], vec![pay_to(&key, 800)]);
        assert!(accept(&mut pool, &set, second).is_ok());
    }

    #[test]
    fn removing_something_that_was_never_held_reports_absence() {
        assert!(Mempool::new()
            .remove(&crate::transaction::Txid::from_bytes([1; 32]))
            .is_none());
    }

    #[test]
    fn the_pool_refuses_a_transaction_past_its_bound_rather_than_growing() {
        let mut set = UtxoSet::new();
        let key = PrivateKey::random();
        let mut pool = Mempool::new();

        // Filled through the field rather than through `accept`: the bound is
        // what is under test, and five thousand real signatures to reach it
        // would make this the slowest test in the suite by two orders.
        for n in 0..MAX_MEMPOOL as u64 {
            let outpoint = funded(&mut set, &key, 1_000 + n, 0);
            let filler = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
            pool.entries.insert(
                filler.get_tx_id(),
                Entry {
                    transaction: filler,
                    fee: Amount::ZERO,
                },
            );
        }

        let outpoint = funded(&mut set, &key, 1_000 + MAX_MEMPOOL as u64, 0);
        let one_more = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);

        assert_eq!(pool.len(), MAX_MEMPOOL);
        assert!(accept(&mut pool, &set, one_more).is_err());
        assert_eq!(pool.len(), MAX_MEMPOOL);
    }

    #[test]
    fn a_transaction_checked_against_coins_a_block_then_spent_is_refused() {
        let (mut set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let payment = signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]);
        let coins = set.coins_for(&payment);

        // A block connects while the signature is being checked, taking the
        // very coin this was validated against.
        let confirmed = signed(&key, &outpoints[..1], vec![pay_to(&key, 800)]);
        set.connect(&confirmed, 1).unwrap();

        let refusal = format!(
            "{:#}",
            pool.admit(
                payment,
                &coins,
                Amount::ZERO,
                &set,
                AFTER_MATURITY,
                &MAINNET
            )
            .unwrap_err()
        );

        assert!(
            refusal.contains("was spent while this was checked"),
            "{refusal}"
        );
        assert!(pool.is_empty());
    }

    #[test]
    fn a_transaction_checked_against_a_different_coin_is_refused() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let payment = signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]);

        // The right outpoint, the wrong coin: not what the set holds.
        let mut lying = set.coins_for(&payment);
        for coin in lying.values_mut() {
            coin.output.value = Amount::from_atoms(9_999_999).unwrap();
        }

        assert!(pool
            .admit(
                payment,
                &lying,
                Amount::ZERO,
                &set,
                AFTER_MATURITY,
                &MAINNET
            )
            .is_err());
    }

    /// The fee the two-phase path carries is the one `check_spend` computed
    /// against these coins, and `admit` confirms every one of them unchanged —
    /// so the number it keeps is the number the set supports.
    #[test]
    fn an_admitted_transaction_keeps_the_fee_its_coins_support() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let payment = signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]);
        let coins = set.coins_for(&payment);
        let fee =
            crate::validation::check_spend(&payment, &coins, AFTER_MATURITY, &MAINNET).unwrap();

        let txid = pool
            .admit(payment, &coins, fee, &set, AFTER_MATURITY, &MAINNET)
            .unwrap();

        assert_eq!(
            pool.remove(&txid).unwrap().fee,
            Amount::from_atoms(100).unwrap()
        );
    }

    #[test]
    fn a_coinbase_that_a_reorg_made_immature_again_is_not_admitted() {
        let mut set = UtxoSet::new();
        let key = PrivateKey::random();
        let outpoint = funded(&mut set, &key, 1_000, 10);
        let mut pool = Mempool::new();
        let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
        let coins = set.coins_for(&payment);
        let mature = 10 + MAINNET.maturity;

        // Checked at a height the chain has since fallen below.
        let fee = crate::validation::check_spend(&payment, &coins, mature, &MAINNET).unwrap();

        assert!(pool
            .admit(payment, &coins, fee, &set, mature - 1, &MAINNET)
            .is_err());
    }

    #[test]
    fn an_immature_coin_is_not_admitted_however_it_was_checked() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let payment = signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]);
        let coins = set.coins_for(&payment);

        assert!(pool
            .admit(
                payment,
                &coins,
                Amount::ZERO,
                &set,
                MAINNET.maturity - 1,
                &MAINNET
            )
            .is_err());
    }

    #[test]
    fn the_fee_a_transaction_pays_is_kept_with_it() {
        let (set, key, outpoints) = a_funded_wallet();
        let mut pool = Mempool::new();
        let transaction = signed(&key, &outpoints[..1], vec![pay_to(&key, 900)]);

        let txid = accept(&mut pool, &set, transaction).unwrap();

        assert_eq!(
            pool.remove(&txid).unwrap().fee,
            Amount::from_atoms(100).unwrap()
        );
    }
}
