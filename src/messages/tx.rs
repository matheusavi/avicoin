use crate::byte_reader::ByteReader;
use crate::messages::message::Payload;
use crate::transaction::Transaction;
use crate::util::command_12;
use anyhow::Result;

pub const TX_COMMAND_NAME: &str = "tx";

/// One transaction, witnesses included — the wire always carries them, which
/// is why the witness-excluded form needs no marker byte (ADR-0003).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tx {
    pub transaction: Transaction,
}

impl Tx {
    pub fn new(transaction: Transaction) -> Self {
        Tx { transaction }
    }

    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<Tx> {
        let mut reader = ByteReader::new(&bytes);

        Ok(Tx {
            transaction: Transaction::parse_raw(&mut reader)?,
        })
    }
}

impl Payload for Tx {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        Ok(self.transaction.get_raw_format())
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(TX_COMMAND_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::Amount;
    use crate::transaction::{Outpoint, TxIn, TxOut, Txid, Witness};

    fn a_transaction() -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![
                TxIn {
                    previous_output: Outpoint {
                        txid: Txid::from_bytes([1; 32]),
                        v_out: 0,
                    },
                    coinbase_data: Vec::new(),
                    witness: Witness::new(vec![vec![0xab; 64], vec![0x02; 33]]),
                },
                TxIn {
                    previous_output: Outpoint {
                        txid: Txid::from_bytes([2; 32]),
                        v_out: 7,
                    },
                    coinbase_data: Vec::new(),
                    witness: Witness::new(vec![vec![0xcd; 64], vec![0x03; 33]]),
                },
            ],
            outputs: vec![
                TxOut {
                    value: Amount::from_atoms(50_000).unwrap(),
                    script_pubkey: vec![0x76, 0xa9, 0x14],
                },
                TxOut {
                    value: Amount::from_atoms(1).unwrap(),
                    script_pubkey: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn a_tx_survives_a_round_trip_with_its_witnesses() {
        let original = Tx::new(a_transaction());

        let parsed = Tx::parse_raw_format(original.get_raw_format().unwrap()).unwrap();

        assert_eq!(original, parsed);
        assert_eq!(
            original.transaction.get_wtxid(),
            parsed.transaction.get_wtxid(),
            "a wire form that dropped a witness would still round-trip the txid"
        );
    }

    #[test]
    fn trailing_bytes_do_not_stop_a_transaction_parsing() {
        let mut padded = Tx::new(a_transaction()).get_raw_format().unwrap();
        padded.push(0);

        assert!(Tx::parse_raw_format(padded).is_ok());
    }
}
