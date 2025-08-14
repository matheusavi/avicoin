use crate::block::Block;
use anyhow::{anyhow, Context, Result};

const MAGIC_BYTES: [u8; 4] = [0xf9, 0xbe, 0xb4, 0xd9];
pub fn frame_block(block: Block) -> Result<Vec<u8>> {
    let mut bytes = block.get_raw_format()?;

    let length = bytes.len() as u32;

    bytes.splice(0..0, length.to_le_bytes());
    bytes.splice(0..0, MAGIC_BYTES);

    Ok(bytes)
}

pub fn unframe_block(bytes: Vec<u8>) -> Result<Block> {
    if bytes[0..4] != MAGIC_BYTES {
        return Err(anyhow!("Invalid magic bytes"));
    }
    let length = u32::from_le_bytes(bytes[4..8].try_into().context("Invalid length")?);
    Block::parse_raw(bytes[8..(length + 8) as usize].to_vec()).context("Failed to unframe block")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::Block;
    use crate::transaction::{Outpoint, Transaction, TxIn, TxOut};

    fn dummy_block() -> Block {
        let mut block = Block::new(
            1,
            [0; 32],
            0,
            0x1d00ffff,
            vec![Transaction {
                version: 1,
                inputs: vec![TxIn {
                    previous_output: Outpoint {
                        tx_id: [0; 32],
                        v_out: 0,
                    },
                }],
                outputs: vec![TxOut {
                    value: 10_000,
                    destiny_pub_key: "12345".to_string(),
                }],
                signature: "my_signature".to_string(),
            }],
        );
        block.mine().unwrap();
        block
    }

    #[test]
    fn test_frame_and_unframe_block() {
        // TODO: Assert values in the block, here we should just test some props and if everything works
        let block = dummy_block();
        let framed = frame_block(block.clone()).expect("Should frame block");
        let unframed = unframe_block(framed).expect("Should unframe block");
        assert_eq!(unframed.version, block.version);
        assert_eq!(unframed.previous_block_hash, block.previous_block_hash);
        assert_eq!(unframed.time, block.time);
        assert_eq!(unframed.n_bits, block.n_bits);
        assert_eq!(unframed.nonce, block.nonce);
        // in the future serialize transactions too
    }

    #[test]
    fn test_unframe_invalid_magic_bytes() {
        let block = dummy_block();
        let mut framed = frame_block(block).unwrap();
        framed[0] = 0x00; // Corrupt magic bytes
        let result = unframe_block(framed);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Invalid magic bytes");
    }
}
