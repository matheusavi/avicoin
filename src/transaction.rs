use crate::byte_reader::ByteReader;
use crate::util::{get_compact_int, get_hash};
use anyhow::{anyhow, Context, Result};

#[derive(Clone, Debug)]
pub struct Transaction {
    pub version: u32,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    pub signature: String,
}

#[derive(Clone, Debug)]
pub struct TxIn {
    pub previous_output: Outpoint,
}

#[derive(Clone, Debug)]
pub struct Outpoint {
    pub tx_id: [u8; 32],
    pub v_out: u32,
}

#[derive(Clone, Debug)]
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

    pub fn get_raw_format(&self) -> Vec<u8> {
        let mut raw_format = Vec::new();
        raw_format.extend(&self.version.to_le_bytes());
        raw_format.extend(get_compact_int(self.inputs.len() as u64));
        for tx in &self.inputs {
            raw_format.extend(tx.previous_output.tx_id);
            raw_format.extend(tx.previous_output.v_out.to_le_bytes());
        }

        raw_format.extend(get_compact_int(self.outputs.len() as u64));
        for tx in &self.outputs {
            raw_format.extend(tx.value.to_le_bytes());
            raw_format.extend(get_compact_int(tx.destiny_pub_key.len() as u64));
            raw_format.extend(tx.destiny_pub_key.as_bytes());
        }

        raw_format.extend(get_compact_int(self.signature.len() as u64));
        raw_format.extend(self.signature.as_bytes());

        raw_format
    }

    pub fn parse_raw(reader: &mut ByteReader) -> Result<Transaction> {
        let version = reader.read_u32()?;
        let input_count = reader.read_compact()?;
        let mut inputs = Vec::with_capacity(input_count as usize);
        // needs to be -1 here at the end, maybe < length something like that
        for _ in 0..input_count {
            let input = TxIn {
                previous_output: {
                    Outpoint {
                        tx_id: reader.read_array::<32>()?,
                        v_out: reader.read_u32()?,
                    }
                },
            };
            inputs.push(input)
        }

        let output_count = reader.read_compact()?;
        let mut outputs = Vec::with_capacity(output_count as usize);
        for _ in 0..output_count {
            let value = reader.read_u64()?;
            let pub_length = reader.read_compact()?;

            let mut string_bytes = Vec::with_capacity(pub_length as usize);
            for _  in 0..pub_length {
                string_bytes.push(reader.read_byte()?)
            }

            let pub_key: String =
                String::from_utf8(string_bytes).context("Invalid utf8 string")?;

            let output = TxOut {
                value,
                destiny_pub_key: pub_key,
            };
            outputs.push(output)
        }
        let signature_length = reader.read_compact()?;

        let mut string_bytes = Vec::with_capacity(signature_length as usize);
        for _ in 0..signature_length {
            string_bytes.push(reader.read_byte()?)
        }

        let signature: String =
            String::from_utf8(string_bytes).context("Invalid utf8 string")?;

        Ok(Transaction {
            version,
            inputs,
            outputs,
            signature,
        })
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
