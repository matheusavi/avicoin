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
