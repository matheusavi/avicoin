use crate::byte_reader::ByteReader;
use crate::util::{get_compact_int, get_hash};
use anyhow::{Context, Result};
use std::fmt;

// The two hashes share an implementation and, deliberately, no type: ADR-0003.
macro_rules! transaction_hash {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            pub fn from_bytes(bytes: [u8; 32]) -> Self {
                $name(bytes)
            }

            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut displayed = self.0;
                displayed.reverse();
                write!(f, "{}", hex::encode(displayed))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({self})", stringify!($name))
            }
        }
    };
}

transaction_hash!(Txid);
transaction_hash!(Wtxid);

#[derive(Clone, Debug)]
pub struct Transaction {
    pub version: u32,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    pub lock_time: u32,
}

#[derive(Clone, Debug)]
pub struct TxIn {
    pub previous_output: Outpoint,
    pub signature: String,
    pub sequence: u32,
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
        get_hash(self.get_raw_format().as_slice())
    }

    pub fn get_raw_format(&self) -> Vec<u8> {
        let mut raw_format = Vec::new();
        raw_format.extend(&self.version.to_le_bytes());
        raw_format.extend(get_compact_int(self.inputs.len() as u64));
        for tx in &self.inputs {
            raw_format.extend(tx.previous_output.tx_id);
            raw_format.extend(tx.previous_output.v_out.to_le_bytes());
            raw_format.extend(get_compact_int(tx.signature.len() as u64));
            raw_format.extend(tx.signature.as_bytes());
            raw_format.extend(tx.sequence.to_le_bytes());
        }

        raw_format.extend(get_compact_int(self.outputs.len() as u64));
        for tx in &self.outputs {
            raw_format.extend(tx.value.to_le_bytes());
            raw_format.extend(get_compact_int(tx.destiny_pub_key.len() as u64));
            raw_format.extend(tx.destiny_pub_key.as_bytes());
        }

        raw_format.extend(self.lock_time.to_le_bytes());

        raw_format
    }

    pub fn parse_raw(reader: &mut ByteReader) -> Result<Transaction> {
        let version = reader.read_u32()?;
        let input_count = reader.read_compact()?;
        let mut inputs = Vec::with_capacity(input_count as usize);
        for _ in 0..input_count {
            let tx_id = reader.read_array::<32>()?;
            let v_out = reader.read_u32()?;
            let signature_length = reader.read_compact()?;

            let mut string_bytes = Vec::with_capacity(signature_length as usize);
            for _ in 0..signature_length {
                string_bytes.push(reader.read_byte()?)
            }

            let signature: String =
                String::from_utf8(string_bytes).context("Invalid utf8 string")?;

            let input = TxIn {
                previous_output: { Outpoint { tx_id, v_out } },
                signature,
                sequence: reader.read_u32()?,
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

        let lock_time = reader.read_u32()?;

        Ok(Transaction {
            version,
            inputs,
            outputs,
            lock_time,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::transaction::{Outpoint, Transaction, TxIn, TxOut, Txid, Wtxid};

    // Bitcoin block 170's second transaction, the first payment ever made.
    const DISPLAYED: &str = "f4184fc596403b9d638783cf57adfe4c75c605f6356fbc91338530e9831e9e16";

    fn wire_order() -> [u8; 32] {
        let mut bytes: [u8; 32] = hex::decode(DISPLAYED).unwrap().try_into().unwrap();
        bytes.reverse();
        bytes
    }

    #[test]
    fn a_txid_serializes_little_endian_and_displays_big_endian() {
        let txid = Txid::from_bytes(wire_order());

        assert_eq!(txid.as_bytes(), &wire_order());
        assert_eq!(txid.to_string(), DISPLAYED);
    }

    #[test]
    fn a_wtxid_displays_the_same_way_a_txid_does() {
        assert_eq!(Wtxid::from_bytes(wire_order()).to_string(), DISPLAYED);
    }

    #[test]
    fn the_two_hashes_say_which_they_are_when_debugged() {
        let bytes = wire_order();

        assert!(format!("{:?}", Txid::from_bytes(bytes)).starts_with("Txid("));
        assert!(format!("{:?}", Wtxid::from_bytes(bytes)).starts_with("Wtxid("));
    }

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
                    signature: "first_signature".to_string(),
                    sequence: 0xFFFFFFFF,
                },
                TxIn {
                    previous_output: Outpoint {
                        tx_id: [255; 32],
                        v_out: 456,
                    },
                    signature: "second_signature".to_string(),
                    sequence: 0xFFFFFFFE,
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
            lock_time: 7890,
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
            original_tx.lock_time, parsed_tx.lock_time,
            "Lock time mismatch"
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
            assert_eq!(
                original_input.signature, parsed_input.signature,
                "Input {} signature mismatch",
                i
            );
            assert_eq!(
                original_input.sequence, parsed_input.sequence,
                "Input {} sequence mismatch",
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
