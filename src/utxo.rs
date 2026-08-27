use crate::amount::Amount;
use crate::transaction::{Outpoint, Transaction, TxOut};
use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// An unspent output and the two facts about its origin that outlive it. The
/// height and the coinbase flag are not extras: restoring a coin during a
/// reorg means re-checking its maturity against the new tip, and that needs
/// the height it was created at — ADR-0012.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coin {
    pub output: TxOut,
    pub height: u32,
    pub from_coinbase: bool,
}

impl Coin {
    /// A coinbase may not be spent until it is `maturity` blocks deep, so a
    /// reorg that orphans it cannot cascade invalidity through its
    /// descendants — ADR-0008.
    pub fn spendable_at(&self, height: u32, maturity: u32) -> bool {
        !self.from_coinbase || height >= self.height.saturating_add(maturity)
    }
}

/// What a block consumed, in the order it consumed it. Written when a block
/// connects and read when it disconnects — ADR-0012.
pub type Undo = Vec<(Outpoint, Coin)>;

#[derive(Debug, Default)]
pub struct UtxoSet {
    coins: HashMap<Outpoint, Coin>,
}

impl UtxoSet {
    pub fn new() -> Self {
        UtxoSet::default()
    }

    /// Owned, not borrowed. A `redb`-backed set reads inside a transaction and
    /// cannot hand out a reference tied to `&self`, so borrowing here would
    /// make M5 a change to every caller rather than to this file — ADR-0013.
    pub fn get(&self, outpoint: &Outpoint) -> Option<Coin> {
        self.coins.get(outpoint).cloned()
    }

    pub fn len(&self) -> usize {
        self.coins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.coins.is_empty()
    }

    pub fn coins(&self) -> Vec<(Outpoint, Coin)> {
        self.coins
            .iter()
            .map(|(outpoint, coin)| (*outpoint, coin.clone()))
            .collect()
    }

    pub fn total(&self) -> Option<Amount> {
        Amount::sum(self.coins.values().map(|coin| coin.output.value))
    }

    /// Spends what the transaction names and creates what it pays, returning
    /// the record `disconnect` needs to put it back.
    pub fn connect(&mut self, transaction: &Transaction, height: u32) -> Result<Undo> {
        let from_coinbase = transaction.is_coinbase();

        let mut spent = Undo::new();
        if !from_coinbase {
            for input in &transaction.inputs {
                let outpoint = input.previous_output;
                let coin = self
                    .coins
                    .remove(&outpoint)
                    .ok_or_else(|| anyhow!("{outpoint:?} is not an unspent output"))?;

                spent.push((outpoint, coin));
            }
        }

        for (index, output) in transaction.outputs.iter().enumerate() {
            self.coins.insert(
                Outpoint {
                    txid: transaction.get_tx_id(),
                    v_out: index as u32,
                },
                Coin {
                    output: output.clone(),
                    height,
                    from_coinbase,
                },
            );
        }

        Ok(spent)
    }

    /// The exact inverse of `connect`, given what `connect` returned.
    pub fn disconnect(&mut self, transaction: &Transaction, spent: &Undo) -> Result<()> {
        let txid = transaction.get_tx_id();
        for index in 0..transaction.outputs.len() {
            let outpoint = Outpoint {
                txid,
                v_out: index as u32,
            };
            self.coins
                .remove(&outpoint)
                .ok_or_else(|| anyhow!("{outpoint:?} was not there to remove"))?;
        }

        for (outpoint, coin) in spent {
            if self.coins.insert(*outpoint, coin.clone()).is_some() {
                return Err(anyhow!("{outpoint:?} was already unspent"));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MAINNET, TESTNET};
    use crate::transaction::{TxIn, Witness};
    use rstest::rstest;

    fn coins(atoms: u64) -> TxOut {
        TxOut {
            value: Amount::from_atoms(atoms).unwrap(),
            script_pubkey: vec![0x51],
        }
    }

    fn coinbase(marker: u8) -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data: vec![0, 0, 0, 0, marker],
                witness: Witness::empty(),
            }],
            outputs: vec![coins(50), coins(7)],
        }
    }

    fn spending(outpoints: &[Outpoint]) -> Transaction {
        Transaction {
            version: 1,
            inputs: outpoints
                .iter()
                .map(|previous_output| TxIn {
                    previous_output: *previous_output,
                    coinbase_data: Vec::new(),
                    witness: Witness::new(vec![vec![1]]),
                })
                .collect(),
            outputs: vec![coins(10)],
        }
    }

    fn sorted(set: &UtxoSet) -> Vec<(Outpoint, Coin)> {
        let mut coins = set.coins();
        coins.sort_by_key(|(outpoint, _)| (outpoint.txid.to_string(), outpoint.v_out));
        coins
    }

    fn out(transaction: &Transaction, v_out: u32) -> Outpoint {
        Outpoint {
            txid: transaction.get_tx_id(),
            v_out,
        }
    }

    #[test]
    fn connecting_a_transaction_creates_every_output_it_pays() {
        let mut set = UtxoSet::new();
        let funding = coinbase(1);

        set.connect(&funding, 3).unwrap();

        assert_eq!(set.len(), 2);
        assert_eq!(set.get(&out(&funding, 0)).unwrap().output, coins(50));
        assert_eq!(set.get(&out(&funding, 1)).unwrap().output, coins(7));
    }

    #[test]
    fn connecting_a_spend_removes_what_it_spends() {
        let mut set = UtxoSet::new();
        let funding = coinbase(1);
        set.connect(&funding, 0).unwrap();
        let spend = spending(&[out(&funding, 0)]);

        set.connect(&spend, 1).unwrap();

        assert!(set.get(&out(&funding, 0)).is_none());
        assert!(set.get(&out(&funding, 1)).is_some());
        assert!(set.get(&out(&spend, 0)).is_some());
    }

    #[test]
    fn an_outpoint_that_was_never_created_cannot_be_spent() {
        let mut set = UtxoSet::new();
        let funding = coinbase(1);

        assert!(set.connect(&spending(&[out(&funding, 0)]), 1).is_err());
    }

    #[test]
    fn an_outpoint_already_spent_cannot_be_spent_again() {
        let mut set = UtxoSet::new();
        let funding = coinbase(1);
        set.connect(&funding, 0).unwrap();
        set.connect(&spending(&[out(&funding, 0)]), 1).unwrap();

        assert!(set.connect(&spending(&[out(&funding, 0)]), 2).is_err());
    }

    #[test]
    fn disconnecting_puts_the_set_back_exactly_as_it_was() {
        let mut set = UtxoSet::new();
        let funding = coinbase(1);
        set.connect(&funding, 0).unwrap();
        let before = sorted(&set);

        let spend = spending(&[out(&funding, 0), out(&funding, 1)]);
        let undo = set.connect(&spend, 1).unwrap();
        set.disconnect(&spend, &undo).unwrap();

        assert_eq!(before, sorted(&set));
    }

    #[test]
    fn a_restored_coin_keeps_the_height_it_was_created_at() {
        let mut set = UtxoSet::new();
        let funding = coinbase(1);
        set.connect(&funding, 9).unwrap();
        let spend = spending(&[out(&funding, 0)]);

        let undo = set.connect(&spend, 40).unwrap();
        set.disconnect(&spend, &undo).unwrap();

        let restored = set.get(&out(&funding, 0)).unwrap();
        assert_eq!(restored.height, 9);
        assert!(restored.from_coinbase, "and that it came from a coinbase");
    }

    #[test]
    fn disconnecting_a_transaction_that_was_never_connected_is_refused() {
        let mut set = UtxoSet::new();

        assert!(set.disconnect(&coinbase(1), &Undo::new()).is_err());
    }

    #[test]
    fn a_coinbase_creates_coins_without_spending_any() {
        let mut set = UtxoSet::new();

        let undo = set.connect(&coinbase(1), 0).unwrap();

        assert!(undo.is_empty(), "a coinbase has nothing to spend");
    }

    #[rstest]
    #[case::the_block_it_was_made_in(0, false)]
    #[case::one_short(99, false)]
    #[case::exactly_deep_enough(100, true)]
    #[case::deeper(1_000, true)]
    fn a_coinbase_output_is_immature_until_it_is_maturity_blocks_deep(
        #[case] height: u32,
        #[case] spendable: bool,
    ) {
        let coin = Coin {
            output: coins(50),
            height: 0,
            from_coinbase: true,
        };

        assert_eq!(coin.spendable_at(height, MAINNET.maturity), spendable);
    }

    #[test]
    fn an_ordinary_output_is_spendable_the_moment_it_exists() {
        let coin = Coin {
            output: coins(50),
            height: 7,
            from_coinbase: false,
        };

        assert!(coin.spendable_at(7, MAINNET.maturity));
    }

    #[test]
    fn the_test_network_lowers_maturity_so_a_test_can_spend() {
        assert!(TESTNET.maturity < MAINNET.maturity);
    }
}
