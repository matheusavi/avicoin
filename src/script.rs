use crate::byte_reader::ByteReader;
use crate::crypto::{verify, PubKeyHash, PublicKey, Signature};
use crate::transaction::{Txid, Witness};
use crate::util::hash160;
use anyhow::{anyhow, bail, Result};
use sha2::{Digest, Sha256};

/// ADR-0002 fixes that there are explicit limits and leaves the numbers to the
/// implementation. These are them.
pub const MAX_SCRIPT_SIZE: usize = 1_000;
pub const MAX_STACK_DEPTH: usize = 100;
pub const MAX_OPERATIONS: usize = 200;
pub const MAX_STACK_ITEM_SIZE: usize = 520;

const OP_FALSE: u8 = 0x00;
const PUSH_MIN: u8 = 0x01;
const PUSH_MAX: u8 = 0x4b;
const OP_TRUE: u8 = 0x51;
const OP_VERIFY: u8 = 0x69;
const OP_DROP: u8 = 0x75;
const OP_DUP: u8 = 0x76;
const OP_SWAP: u8 = 0x7c;
const OP_EQUAL: u8 = 0x87;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_SHA256: u8 = 0xa8;
const OP_HASH160: u8 = 0xa9;
const OP_CHECKSIG: u8 = 0xac;
const OP_CHECKSIGVERIFY: u8 = 0xad;

pub fn p2pkh(hash: &PubKeyHash) -> Vec<u8> {
    let mut script = vec![OP_DUP, OP_HASH160, hash.as_bytes().len() as u8];
    script.extend(hash.as_bytes());
    script.extend([OP_EQUALVERIFY, OP_CHECKSIG]);
    script
}

/// Runs `script_pubkey` over a stack the witness has already seeded. `Ok`
/// means the output is unlocked; the error says why it is not.
///
/// ADR-0002 sketched this as `Result<bool>`. Nothing distinguishes "ran and
/// left something falsy" from "could not run" — both mean the coin stays put —
/// so the two collapse into one channel that carries a reason.
pub fn execute(script_pubkey: &[u8], witness: &Witness, txid: Txid) -> Result<()> {
    if script_pubkey.len() > MAX_SCRIPT_SIZE {
        bail!(
            "a script of {} bytes is over the {MAX_SCRIPT_SIZE}-byte limit",
            script_pubkey.len()
        );
    }

    let mut machine = Machine::seeded_by(witness)?;
    let mut reader = ByteReader::new(script_pubkey);

    while reader.remaining() > 0 {
        machine.step(reader.read_byte()?, &mut reader, txid)?;
    }

    machine.result()
}

struct Machine {
    stack: Vec<Vec<u8>>,
    operations: usize,
}

impl Machine {
    fn seeded_by(witness: &Witness) -> Result<Machine> {
        let mut machine = Machine {
            stack: Vec::new(),
            operations: 0,
        };

        for item in witness.items() {
            machine.push(item.clone())?;
        }

        Ok(machine)
    }

    fn step(&mut self, opcode: u8, reader: &mut ByteReader, txid: Txid) -> Result<()> {
        if let PUSH_MIN..=PUSH_MAX = opcode {
            return self.push(reader.read_bytes(opcode as usize)?);
        }

        self.operations += 1;
        if self.operations > MAX_OPERATIONS {
            bail!("a script may run {MAX_OPERATIONS} operations");
        }

        match opcode {
            OP_FALSE => self.push(Vec::new()),
            OP_TRUE => self.push(vec![1]),
            OP_DROP => self.pop().map(drop),
            OP_DUP => {
                let top = self.top()?.clone();
                self.push(top)
            }
            OP_SWAP => {
                let (first, second) = (self.pop()?, self.pop()?);
                self.push(first)?;
                self.push(second)
            }
            OP_SHA256 => {
                let item = self.pop()?;
                self.push(Sha256::digest(&item).to_vec())
            }
            OP_HASH160 => {
                let item = self.pop()?;
                self.push(hash160(&item).to_vec())
            }
            OP_EQUAL => {
                let equal = self.pop()? == self.pop()?;
                self.push(boolean(equal))
            }
            OP_EQUALVERIFY => {
                if self.pop()? != self.pop()? {
                    bail!("OP_EQUALVERIFY: the two items differ");
                }
                Ok(())
            }
            OP_VERIFY => {
                if !is_truthy(&self.pop()?) {
                    bail!("OP_VERIFY: the top of the stack is false");
                }
                Ok(())
            }
            OP_CHECKSIG => {
                let verified = self.check_signature(txid)?;
                self.push(boolean(verified))
            }
            OP_CHECKSIGVERIFY => {
                if !self.check_signature(txid)? {
                    bail!("OP_CHECKSIGVERIFY: the signature does not match");
                }
                Ok(())
            }
            unknown => bail!("{unknown:#04x} is not an opcode"),
        }
    }

    fn check_signature(&mut self, txid: Txid) -> Result<bool> {
        let public_key = PublicKey::parse(&self.pop()?);
        let signature = Signature::parse(&self.pop()?);

        match (public_key, signature) {
            (Ok(public_key), Ok(signature)) => Ok(verify(&signature, txid.as_bytes(), &public_key)),
            _ => Ok(false),
        }
    }

    fn push(&mut self, item: Vec<u8>) -> Result<()> {
        if item.len() > MAX_STACK_ITEM_SIZE {
            bail!(
                "a stack item of {} bytes is over the {MAX_STACK_ITEM_SIZE}-byte limit",
                item.len()
            );
        }
        if self.stack.len() == MAX_STACK_DEPTH {
            bail!("the stack may hold {MAX_STACK_DEPTH} items");
        }

        self.stack.push(item);
        Ok(())
    }

    fn pop(&mut self) -> Result<Vec<u8>> {
        self.stack
            .pop()
            .ok_or_else(|| anyhow!("the stack is empty"))
    }

    fn top(&self) -> Result<&Vec<u8>> {
        self.stack
            .last()
            .ok_or_else(|| anyhow!("the stack is empty"))
    }

    fn result(self) -> Result<()> {
        let [only] = &self.stack[..] else {
            bail!("a script leaves one item, this left {}", self.stack.len());
        };

        if !is_truthy(only) {
            bail!("the script left a false result");
        }

        Ok(())
    }
}

fn boolean(value: bool) -> Vec<u8> {
    if value {
        vec![1]
    } else {
        Vec::new()
    }
}

// Empty and all-zero are false. There is no negative zero to worry about:
// Bitcoin's exists only because its numbers are sign-magnitude, and ours has
// no numeric opcodes at all.
fn is_truthy(item: &[u8]) -> bool {
    item.iter().any(|&byte| byte != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{PrivateKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
    use rstest::rstest;

    fn txid(byte: u8) -> Txid {
        Txid::from_bytes([byte; 32])
    }

    fn p2pkh_for(key: &PrivateKey) -> Vec<u8> {
        p2pkh(&PubKeyHash::from_bytes(hash160(
            key.public_key().as_bytes(),
        )))
    }

    fn unlocks(key: &PrivateKey, over: Txid) -> Witness {
        Witness::new(vec![
            key.sign(over.as_bytes()).as_bytes().to_vec(),
            key.public_key().as_bytes().to_vec(),
        ])
    }

    #[test]
    fn the_template_unlocks_for_the_key_it_names() {
        let key = PrivateKey::random();

        assert!(execute(&p2pkh_for(&key), &unlocks(&key, txid(1)), txid(1)).is_ok());
    }

    #[test]
    fn the_template_is_the_five_opcodes_adr_0002_names() {
        let script = p2pkh(&PubKeyHash::from_bytes([9; 20]));

        assert_eq!(script[0], OP_DUP);
        assert_eq!(script[1], OP_HASH160);
        assert_eq!(script[2], 20);
        assert_eq!(&script[3..23], &[9; 20]);
        assert_eq!(&script[23..], &[OP_EQUALVERIFY, OP_CHECKSIG]);
    }

    #[test]
    fn another_keys_signature_does_not_unlock_it() {
        let owner = PrivateKey::random();
        let stranger = PrivateKey::random();

        assert!(execute(&p2pkh_for(&owner), &unlocks(&stranger, txid(1)), txid(1)).is_err());
    }

    #[test]
    fn a_signature_over_another_transaction_does_not_unlock_it() {
        let key = PrivateKey::random();

        assert!(execute(&p2pkh_for(&key), &unlocks(&key, txid(1)), txid(2)).is_err());
    }

    #[test]
    fn a_high_s_signature_does_not_unlock_it() {
        use k256::ecdsa::Signature as EcdsaSignature;

        let key = PrivateKey::random();
        let signature = key.sign(txid(1).as_bytes());
        let parsed = EcdsaSignature::from_slice(signature.as_bytes()).unwrap();
        let (r, s) = parsed.split_scalars();
        let twin: [u8; SIGNATURE_LEN] = EcdsaSignature::from_scalars(*r, -*s)
            .unwrap()
            .to_bytes()
            .into();

        let witness = Witness::new(vec![twin.to_vec(), key.public_key().as_bytes().to_vec()]);

        assert!(execute(&p2pkh_for(&key), &witness, txid(1)).is_err());
    }

    #[test]
    fn an_uncompressed_public_key_does_not_unlock_it() {
        let key = PrivateKey::random();
        let uncompressed = k256::ecdsa::VerifyingKey::from_sec1_bytes(key.public_key().as_bytes())
            .unwrap()
            .to_sec1_point(false)
            .as_bytes()
            .to_vec();

        // The hash of the uncompressed form, so only the encoding rule can fail it.
        let script = p2pkh(&PubKeyHash::from_bytes(hash160(&uncompressed)));
        let witness = Witness::new(vec![
            key.sign(txid(1).as_bytes()).as_bytes().to_vec(),
            uncompressed,
        ]);

        assert!(execute(&script, &witness, txid(1)).is_err());
    }

    #[rstest]
    #[case::short_signature(SIGNATURE_LEN - 1, PUBLIC_KEY_LEN)]
    #[case::long_signature(SIGNATURE_LEN + 1, PUBLIC_KEY_LEN)]
    #[case::short_key(SIGNATURE_LEN, PUBLIC_KEY_LEN - 1)]
    #[case::long_key(SIGNATURE_LEN, PUBLIC_KEY_LEN + 1)]
    fn a_witness_item_of_the_wrong_length_does_not_unlock_it(
        #[case] signature: usize,
        #[case] key: usize,
    ) {
        let owner = PrivateKey::random();
        let witness = Witness::new(vec![vec![0xab; signature], vec![0x02; key]]);

        assert!(execute(&p2pkh_for(&owner), &witness, txid(1)).is_err());
    }

    fn preimage_lock(secret: &[u8]) -> Vec<u8> {
        let digest = Sha256::digest(secret);
        let mut script = vec![OP_SHA256, digest.len() as u8];
        script.extend(digest);
        script.push(OP_EQUAL);
        script
    }

    #[test]
    fn a_hash_preimage_lock_opens_for_the_preimage() {
        let script = preimage_lock(b"open sesame");
        let witness = Witness::new(vec![b"open sesame".to_vec()]);

        assert!(execute(&script, &witness, txid(1)).is_ok());
    }

    #[test]
    fn a_hash_preimage_lock_stays_shut_without_it() {
        let script = preimage_lock(b"open sesame");
        let witness = Witness::new(vec![b"open barley".to_vec()]);

        assert!(execute(&script, &witness, txid(1)).is_err());
    }

    #[rstest]
    #[case::reserved(0x50)]
    #[case::op_if(0x63)]
    #[case::op_add(0x93)]
    #[case::op_checkmultisig(0xae)]
    #[case::pushdata1(0x4c)]
    fn an_unknown_opcode_fails_the_script(#[case] opcode: u8) {
        let witness = Witness::new(vec![vec![1]]);

        assert!(execute(&[opcode], &witness, txid(1)).is_err());
    }

    #[test]
    fn a_push_running_past_the_end_of_the_script_fails() {
        assert!(execute(&[0x05, 1, 2], &Witness::empty(), txid(1)).is_err());
    }

    #[rstest]
    #[case::nothing_left(&[OP_TRUE, OP_DROP])]
    #[case::two_left(&[OP_TRUE, OP_TRUE])]
    #[case::one_false(&[OP_FALSE])]
    #[case::one_all_zero(&[0x02, 0x00, 0x00])]
    fn a_script_that_does_not_leave_one_truthy_item_fails(#[case] script: &[u8]) {
        assert!(execute(script, &Witness::empty(), txid(1)).is_err());
    }

    #[test]
    fn an_empty_script_over_an_empty_witness_fails() {
        assert!(execute(&[], &Witness::empty(), txid(1)).is_err());
    }

    /// Pushes cost no operations, so this fills the script with them and drops
    /// all but one — the size limit is then the only thing left to trip.
    fn a_script_of(size: usize) -> Vec<u8> {
        let push = |data: u8| [&[PUSH_MAX][..], &vec![data; PUSH_MAX as usize]].concat();
        let pushes = size / (PUSH_MAX as usize + 1);

        let mut script: Vec<u8> = (0..pushes as u8).flat_map(|n| push(n + 1)).collect();
        script.extend(vec![OP_DROP; size - script.len()]);

        assert_eq!(script.len(), size);
        script
    }

    #[test]
    fn a_script_at_the_size_limit_runs_and_one_byte_over_does_not() {
        let at_limit = a_script_of(MAX_SCRIPT_SIZE);
        let over = a_script_of(MAX_SCRIPT_SIZE + 1);

        assert!(execute(&at_limit, &Witness::empty(), txid(1)).is_ok());
        assert!(execute(&over, &Witness::empty(), txid(1))
            .is_err_and(|error| error.to_string().contains("byte limit")));
    }

    /// `OP_SWAP` leaves the stack the depth it found it, so a run of them
    /// costs operations and nothing else. The final `OP_DROP` leaves one item.
    fn a_script_running(operations: usize) -> Vec<u8> {
        std::iter::repeat_n(OP_SWAP, operations - 1)
            .chain([OP_DROP])
            .collect()
    }

    #[test]
    fn the_last_allowed_operation_runs_and_the_next_one_does_not() {
        let two_items = Witness::new(vec![vec![1], vec![1]]);

        assert!(execute(&a_script_running(MAX_OPERATIONS), &two_items, txid(1)).is_ok());
        assert!(
            execute(&a_script_running(MAX_OPERATIONS + 1), &two_items, txid(1))
                .is_err_and(|error| error.to_string().contains("operations"))
        );
    }

    #[test]
    fn the_stack_depth_is_what_stops_a_deep_script() {
        let to_depth = |depth: usize| -> Vec<u8> {
            [OP_TRUE]
                .into_iter()
                .chain(std::iter::repeat_n(OP_DUP, depth - 1))
                .collect()
        };

        assert!(
            execute(&to_depth(MAX_STACK_DEPTH), &Witness::empty(), txid(1))
                .is_err_and(|error| error.to_string().contains("one item"))
        );
        assert!(
            execute(&to_depth(MAX_STACK_DEPTH + 1), &Witness::empty(), txid(1))
                .is_err_and(|error| error.to_string().contains("items"))
        );
    }

    #[test]
    fn a_witness_item_over_the_size_limit_never_reaches_the_script() {
        let oversized = Witness::new(vec![vec![0; MAX_STACK_ITEM_SIZE + 1]]);
        let allowed = Witness::new(vec![vec![1; MAX_STACK_ITEM_SIZE]]);

        assert!(execute(&[], &oversized, txid(1)).is_err());
        assert!(execute(&[], &allowed, txid(1)).is_ok());
    }

    #[test]
    fn a_witness_deeper_than_the_stack_never_reaches_the_script() {
        let items = vec![vec![1]; MAX_STACK_DEPTH + 1];

        assert!(execute(&[], &Witness::new(items), txid(1)).is_err());
    }

    #[test]
    fn an_operation_on_an_empty_stack_fails_rather_than_panicking() {
        for opcode in [OP_DUP, OP_DROP, OP_SWAP, OP_HASH160, OP_SHA256, OP_VERIFY] {
            assert!(execute(&[opcode], &Witness::empty(), txid(1)).is_err());
        }
    }

    #[test]
    fn swap_puts_the_second_item_on_top() {
        let script = [OP_SWAP, OP_DROP];
        let witness = Witness::new(vec![vec![0], vec![1]]);

        assert!(execute(&script, &witness, txid(1)).is_ok());
    }
}
