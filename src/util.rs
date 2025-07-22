use hex::decode;
use sha256::digest;

pub fn get_hash(slice: &[u8]) -> Vec<u8> {
    let pass1_hex = digest(slice);
    let pass1_raw = decode(pass1_hex).expect("Failed to decode pass 1");

    let pass2_hex = digest(pass1_raw);
    let mut pass2_raw = decode(pass2_hex).expect("Failed to decode pass 2");

    pass2_raw.reverse();

    pass2_raw
}

pub fn get_compact_int(number: u64) -> Vec<u8> {
    match number {
        ..=252 => (number as u8).to_le_bytes().to_vec(),
        253..=0xffff => {
            let mut result = vec![0xfd];
            result.extend((number as u16).to_le_bytes().to_vec());
            result
        }
        0x10000..=0xffff_ffff => {
            let mut result = vec![0xfe];
            result.extend((number as u32).to_le_bytes().to_vec());
            result
        }
        _ => {
            let mut result = vec![0xff];
            result.extend(number.to_le_bytes().to_vec());
            result
        }
    }
}
