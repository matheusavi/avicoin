use crate::block::Block;

const MAGIC_BYTES: [u8; 4] = [0xf9, 0xbe, 0xb4, 0xd9];
pub fn frame_block(block: Block) -> Result<Vec<u8>, String> {
    let mut bytes = block.get_raw_format()?;

    let length = bytes.len() as u32;

    bytes.splice(0..0, length.to_le_bytes());
    bytes.splice(0..0, MAGIC_BYTES);

    Ok(bytes)
}

pub fn unframe_block(bytes: Vec<u8>) -> Result<Block, String> {
    if bytes[0..4] != MAGIC_BYTES {
        return Err(String::from("Invalid magic bytes"));
    }
    let length = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| String::from("Invalid length"))?,
    );
    Block::parse_raw(bytes[8..length as usize].to_vec())
}
