use crate::util::get_hash;

pub struct Transaction {
    pub version: u32,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    pub signature: String,
}
pub struct TxIn {
    pub previous_output: Outpoint,
}

pub struct Outpoint {
    pub tx_id: [u8; 32],
    pub v_out: u32,
}

pub struct TxOut {
    pub value: u64,
    pub destiny_pub_key: String,
}

impl Transaction {
    pub fn get_tx_id(&self) -> [u8; 32] {
        get_hash(&self.get_raw_format().as_slice())
            .try_into()
            .expect("Expected 32 sized array")
    }

    fn get_raw_format(&self) -> Vec<u8> {
        let mut raw_format = Vec::new();
        raw_format.extend(&self.version.to_le_bytes());
        for tx in &self.inputs {
            raw_format.extend(tx.previous_output.tx_id);
            raw_format.extend(tx.previous_output.v_out.to_le_bytes());
        }

        for tx in &self.outputs {
            raw_format.extend(tx.value.to_le_bytes());
            raw_format.extend(tx.destiny_pub_key.as_bytes());
        }

        raw_format.extend(self.signature.as_bytes());

        raw_format
    }
}

#[cfg(test)]
mod tests {
    use crate::transaction::{Outpoint, Transaction, TxIn, TxOut};
    use hex::decode;

    #[test]
    fn get_txn_hash_correct_hash() {
        let tx = Transaction {
            version: 1,
            inputs: {
                vec![TxIn {
                    previous_output: {
                        Outpoint {
                            tx_id: [0; 32],
                            v_out: 0,
                        }
                    },
                }]
            },
            outputs: vec![TxOut {
                value: 10_000,
                destiny_pub_key: "12345".to_string(),
            }],
            signature: "my_signature".to_string(),
        };
        assert_eq!(
            tx.get_tx_id(),
            decode("42671067b1b40eb72ecffb15a91d388c688fbf38fac79c88bc801fe1641b0ede")
                .unwrap()
                .as_slice()
        )
    }
}
