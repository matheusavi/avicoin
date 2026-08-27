use crate::amount::Amount;
use crate::byte_reader::ByteReader;
use crate::util::{get_compact_int, get_hash, hash_newtype};
use anyhow::Result;
hash_newtype!(Txid);
hash_newtype!(Wtxid);

// The smallest each can encode to, which is what bounds a claimed count.
const MIN_TX_IN_SIZE: usize = 32 + 4 + 1 + 1;
const MIN_TX_OUT_SIZE: usize = 8 + 1;
pub const MIN_TRANSACTION_SIZE: usize = 4 + 1 + 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub version: u32,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxIn {
    pub previous_output: Outpoint,
    pub coinbase_data: Vec<u8>,
    pub witness: Witness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Outpoint {
    pub txid: Txid,
    pub v_out: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxOut {
    pub value: Amount,
    pub script_pubkey: Vec<u8>,
}

/// Stack items, not a script — so "push only" is the type rather than a rule.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Witness(Vec<Vec<u8>>);

impl Witness {
    pub fn new(items: Vec<Vec<u8>>) -> Self {
        Witness(items)
    }

    pub fn empty() -> Self {
        Witness(Vec::new())
    }

    pub fn items(&self) -> &[Vec<u8>] {
        &self.0
    }
}

impl Outpoint {
    /// What a coinbase's single input points at.
    pub fn null() -> Self {
        Outpoint {
            txid: Txid::from_bytes([0; 32]),
            v_out: u32::MAX,
        }
    }

    pub fn is_null(&self) -> bool {
        *self == Outpoint::null()
    }
}

impl Transaction {
    /// The one transaction a block creates rather than relays. `coinbase_data`
    /// opens with the height at a fixed offset, which is what keeps two
    /// coinbases from sharing a txid, and continues with an extranonce the
    /// miner grinds for fresh search space — ADR-0008.
    pub fn coinbase(height: u32, extranonce: u64, outputs: Vec<TxOut>) -> Transaction {
        let mut coinbase_data = height.to_le_bytes().to_vec();
        coinbase_data.extend(extranonce.to_le_bytes());

        Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data,
                witness: Witness::empty(),
            }],
            outputs,
        }
    }

    /// ADR-0008 identifies a coinbase by predicate: one input, pointing at no
    /// previous output.
    pub fn is_coinbase(&self) -> bool {
        self.inputs.len() == 1 && self.inputs[0].previous_output.is_null()
    }

    pub fn get_tx_id(&self) -> Txid {
        Txid::from_bytes(get_hash(&self.serialize(false)))
    }

    pub fn get_wtxid(&self) -> Wtxid {
        Wtxid::from_bytes(get_hash(&self.serialize(true)))
    }

    pub fn get_raw_format(&self) -> Vec<u8> {
        self.serialize(true)
    }

    // The witness-excluded form is only ever hashed, so it needs no marker byte
    // and no caller outside the two hashes above.
    fn serialize(&self, include_witness: bool) -> Vec<u8> {
        let mut raw_format = Vec::new();
        raw_format.extend(self.version.to_le_bytes());

        raw_format.extend(get_compact_int(self.inputs.len() as u64));
        for input in &self.inputs {
            raw_format.extend(input.previous_output.txid.as_bytes());
            raw_format.extend(input.previous_output.v_out.to_le_bytes());
            raw_format.extend(var_bytes(&input.coinbase_data));

            if include_witness {
                raw_format.extend(get_compact_int(input.witness.0.len() as u64));
                for item in &input.witness.0 {
                    raw_format.extend(var_bytes(item));
                }
            }
        }

        raw_format.extend(get_compact_int(self.outputs.len() as u64));
        for output in &self.outputs {
            raw_format.extend(output.value.atoms().to_le_bytes());
            raw_format.extend(var_bytes(&output.script_pubkey));
        }

        raw_format
    }

    pub fn parse_raw(reader: &mut ByteReader) -> Result<Transaction> {
        let version = reader.read_u32()?;

        let mut inputs = Vec::new();
        for _ in 0..reader.read_count(MIN_TX_IN_SIZE)? {
            inputs.push(TxIn {
                previous_output: Outpoint {
                    txid: Txid::from_bytes(reader.read_array::<32>()?),
                    v_out: reader.read_u32()?,
                },
                coinbase_data: reader.read_var_bytes()?,
                witness: parse_witness(reader)?,
            });
        }

        let mut outputs = Vec::new();
        for _ in 0..reader.read_count(MIN_TX_OUT_SIZE)? {
            outputs.push(TxOut {
                value: Amount::from_atoms(reader.read_u64()?)?,
                script_pubkey: reader.read_var_bytes()?,
            });
        }

        Ok(Transaction {
            version,
            inputs,
            outputs,
        })
    }
}

fn var_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = get_compact_int(bytes.len() as u64);
    encoded.extend(bytes);
    encoded
}

fn parse_witness(reader: &mut ByteReader) -> Result<Witness> {
    let mut items = Vec::new();
    for _ in 0..reader.read_count(1)? {
        items.push(reader.read_var_bytes()?);
    }

    Ok(Witness(items))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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

    fn spend(v_out: u32, witness: Witness) -> TxIn {
        TxIn {
            previous_output: Outpoint {
                txid: Txid::from_bytes(wire_order()),
                v_out,
            },
            coinbase_data: Vec::new(),
            witness,
        }
    }

    fn signed() -> Witness {
        Witness::new(vec![vec![0xab; 64], vec![0x02; 33]])
    }

    fn pay(atoms: u64) -> TxOut {
        TxOut {
            value: Amount::from_atoms(atoms).unwrap(),
            script_pubkey: vec![0x76, 0xa9, 0x14],
        }
    }

    fn a_transaction() -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![spend(0, signed()), spend(7, Witness::new(vec![vec![1]]))],
            outputs: vec![pay(50_000), pay(1)],
        }
    }

    fn parse(bytes: &[u8]) -> Result<Transaction> {
        Transaction::parse_raw(&mut ByteReader::new(bytes))
    }

    #[test]
    fn a_transaction_survives_serialization_and_parsing() {
        let original = a_transaction();

        assert_eq!(parse(&original.get_raw_format()).unwrap(), original);
    }

    #[test]
    fn a_coinbase_opens_with_its_height_and_survives_the_round_trip() {
        let original = Transaction::coinbase(42, 7, vec![pay(50)]);

        assert!(original.is_coinbase());
        assert_eq!(
            &original.inputs[0].coinbase_data[..4],
            &42u32.to_le_bytes(),
            "the height sits at a fixed offset so parsing it is unambiguous"
        );
        assert_eq!(parse(&original.get_raw_format()).unwrap(), original);
    }

    #[test]
    fn two_coinbases_at_different_heights_do_not_share_a_txid() {
        let outputs = vec![pay(50)];

        assert_ne!(
            Transaction::coinbase(1, 0, outputs.clone()).get_tx_id(),
            Transaction::coinbase(2, 0, outputs.clone()).get_tx_id(),
        );
        assert_ne!(
            Transaction::coinbase(1, 0, outputs.clone()).get_tx_id(),
            Transaction::coinbase(1, 1, outputs).get_tx_id(),
            "and the extranonce moves it too, which is what it is for"
        );
    }

    #[test]
    fn a_coinbase_shaped_input_survives_the_round_trip() {
        let original = Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data: vec![0x2a, 0, 0, 0, b'h', b'i'],
                witness: Witness::empty(),
            }],
            outputs: vec![pay(50 * 100_000_000)],
        };

        let parsed = parse(&original.get_raw_format()).unwrap();

        assert_eq!(parsed, original);
        assert!(parsed.inputs[0].previous_output.is_null());
    }

    #[test]
    fn the_txid_ignores_the_witness_and_the_wtxid_does_not() {
        let original = a_transaction();
        let mut reworded = original.clone();
        reworded.inputs[0].witness = Witness::new(vec![vec![0xcd; 64], vec![0x03; 33]]);

        assert_eq!(original.get_tx_id(), reworded.get_tx_id());
        assert_ne!(original.get_wtxid(), reworded.get_wtxid());
    }

    #[test]
    fn the_witness_excluded_form_is_for_hashing_and_not_for_the_wire() {
        let transaction = a_transaction();

        assert!(transaction.serialize(false).len() < transaction.get_raw_format().len());
        assert!(
            parse(&transaction.serialize(false)).is_err(),
            "no marker byte tells the two apart, so the excluded form is never transmitted"
        );
    }

    #[test]
    fn the_coinbase_data_moves_the_txid() {
        let original = a_transaction();
        let mut marked = original.clone();
        marked.inputs[0].coinbase_data = vec![9];

        assert_ne!(original.get_tx_id(), marked.get_tx_id());
    }

    /// Each of these claims more elements than the bytes behind it could hold.
    /// A parser that reserved on the claim would abort the process rather than
    /// fail this assertion, so the test passing at all is the guarantee.
    #[rstest]
    #[case::inputs(&[1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])]
    #[case::outputs(&[1, 0, 0, 0, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])]
    fn a_count_no_input_could_hold_is_refused(#[case] bytes: &[u8]) {
        assert!(parse(bytes).is_err());
    }

    #[test]
    fn a_witness_claiming_more_items_than_bytes_remain_is_refused() {
        let mut bytes = vec![1, 0, 0, 0, 1];
        bytes.extend([0; 32]);
        bytes.extend(0u32.to_le_bytes());
        bytes.push(0);
        bytes.extend([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);

        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn an_item_longer_than_the_bytes_behind_it_is_refused() {
        let mut bytes = vec![1, 0, 0, 0, 1];
        bytes.extend([0; 32]);
        bytes.extend(0u32.to_le_bytes());
        bytes.extend([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);

        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn a_transaction_with_many_elements_still_parses() {
        let original = Transaction {
            version: 1,
            inputs: (0..400).map(|i| spend(i, signed())).collect(),
            outputs: (0..400).map(|i| pay(i + 1)).collect(),
        };

        assert_eq!(parse(&original.get_raw_format()).unwrap(), original);
    }

    #[test]
    fn an_output_above_max_money_does_not_parse() {
        let mut bytes = vec![1, 0, 0, 0, 0, 1];
        bytes.extend(u64::MAX.to_le_bytes());
        bytes.push(0);

        assert!(parse(&bytes).is_err());
    }
}
