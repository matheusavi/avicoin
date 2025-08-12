use crate::byte_reader::ByteReader;
use crate::util::{get_compact_int, get_hash};
use anyhow::{Context, Result};

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
            for _ in 0..pub_length {
                string_bytes.push(reader.read_byte()?)
            }

            let pub_key: String = String::from_utf8(string_bytes).context("Invalid utf8 string")?;

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

        let signature: String = String::from_utf8(string_bytes).context("Invalid utf8 string")?;

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

    #[test]
    fn test_transaction_round_trip_conversion() {
        use crate::byte_reader::ByteReader;

        let original_tx = Transaction {
            version: 42,
            inputs: vec![
                TxIn {
                    previous_output: Outpoint {
                        tx_id: [0; 32],
                        v_out: 123,
                    },
                },
                TxIn {
                    previous_output: Outpoint {
                        tx_id: [255; 32],
                        v_out: 456,
                    },
                },
            ],
            outputs: vec![
                TxOut {
                    value: 1_000_000,
                    destiny_pub_key: "first_public_key".to_string(),
                },
                TxOut {
                    value: 500_000,
                    destiny_pub_key: "second_public_key_longer".to_string(),
                },
            ],
            signature: "test_signature_data".to_string(),
        };

        let raw_data = original_tx.get_raw_format();

        let mut reader = ByteReader::new(&raw_data);
        let parsed_tx = Transaction::parse_raw(&mut reader).expect("Failed to parse transaction");

        assert_eq!(original_tx.version, parsed_tx.version, "Version mismatch");
        assert_eq!(
            original_tx.inputs.len(),
            parsed_tx.inputs.len(),
            "Input count mismatch"
        );
        assert_eq!(
            original_tx.outputs.len(),
            parsed_tx.outputs.len(),
            "Output count mismatch"
        );
        assert_eq!(
            original_tx.signature, parsed_tx.signature,
            "Signature mismatch"
        );

        for (i, (original_input, parsed_input)) in original_tx
            .inputs
            .iter()
            .zip(parsed_tx.inputs.iter())
            .enumerate()
        {
            assert_eq!(
                original_input.previous_output.tx_id, parsed_input.previous_output.tx_id,
                "Input {} tx_id mismatch",
                i
            );
            assert_eq!(
                original_input.previous_output.v_out, parsed_input.previous_output.v_out,
                "Input {} v_out mismatch",
                i
            );
        }

        for (i, (original_output, parsed_output)) in original_tx
            .outputs
            .iter()
            .zip(parsed_tx.outputs.iter())
            .enumerate()
        {
            assert_eq!(
                original_output.value, parsed_output.value,
                "Output {} value mismatch",
                i
            );
            assert_eq!(
                original_output.destiny_pub_key, parsed_output.destiny_pub_key,
                "Output {} destiny_pub_key mismatch",
                i
            );
        }
    }
}
