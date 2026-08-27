use crate::block::Block;
use crate::messages::message::Payload;
use crate::util::command_12;
use anyhow::Result;

pub const BLOCK_COMMAND_NAME: &str = "block";

/// One whole block: its eighty-byte header, a count, and that many
/// transactions with their witnesses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockMessage {
    pub block: Block,
}

impl BlockMessage {
    pub fn new(block: Block) -> Self {
        BlockMessage { block }
    }

    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<BlockMessage> {
        Ok(BlockMessage {
            block: Block::parse_raw(bytes)?,
        })
    }
}

impl Payload for BlockMessage {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        self.block.get_raw_format()
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(BLOCK_COMMAND_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::subsidy;
    use crate::crypto::PrivateKey;
    use crate::crypto::PubKeyHash;
    use crate::params::TESTNET;
    use crate::script::p2pkh;
    use crate::transaction::{Transaction, TxOut};
    use crate::util::hash160;

    fn a_mined_block(transactions: usize) -> Block {
        let key = PrivateKey::random();
        let paying = |atoms: u64| TxOut {
            value: crate::amount::Amount::from_atoms(atoms).unwrap(),
            script_pubkey: p2pkh(&PubKeyHash::from_bytes(hash160(
                key.public_key().as_bytes(),
            ))),
        };

        let mut block = Block::new(
            1,
            [7; 32],
            1_756_252_800,
            TESTNET.starting_bits,
            (0..transactions as u64)
                .map(|n| Transaction::coinbase(1, n, vec![paying(subsidy(1).atoms())]))
                .collect(),
        );
        assert!(block.mine().unwrap());

        block
    }

    #[test]
    fn a_block_survives_a_round_trip() {
        let original = BlockMessage::new(a_mined_block(3));

        let parsed = BlockMessage::parse_raw_format(original.get_raw_format().unwrap()).unwrap();

        assert_eq!(parsed, original);
        assert_eq!(
            parsed.block.header().unwrap(),
            original.block.header().unwrap(),
            "and the header it round-trips through is the one proof-of-work covers"
        );
    }

    #[test]
    fn a_block_of_one_transaction_survives_a_round_trip() {
        let original = BlockMessage::new(a_mined_block(1));

        assert_eq!(
            BlockMessage::parse_raw_format(original.get_raw_format().unwrap()).unwrap(),
            original
        );
    }

    #[test]
    fn a_truncated_block_does_not_parse() {
        let mut raw = BlockMessage::new(a_mined_block(2))
            .get_raw_format()
            .unwrap();
        raw.truncate(raw.len() - 1);

        assert!(BlockMessage::parse_raw_format(raw).is_err());
    }
}
