use crate::block::Block;

const MAGIC_BYTES: [u8; 4] = [0xf9, 0xbe, 0xb4, 0xd9];
pub fn frame_block(block: Block) -> Result<Vec<u8>, String> {
    let mut bytes = block.get_raw_format()?;

    let length = bytes.len() as u32;

    bytes.splice(0..0, length.to_le_bytes());
    bytes.splice(0..0, MAGIC_BYTES);

    Ok(bytes)
}
