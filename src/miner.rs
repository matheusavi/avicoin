use hex::{decode, encode};
use sha256::digest;
// use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn mine() -> String {
    let nonce = "1dac2b7c";
    let block_header = get_pre_header() + nonce;
    let hex_block_header = decode(&block_header).expect("Failed to decode block header");

    let pass1_hex = digest(hex_block_header);
    let pass1_raw = decode(pass1_hex).expect("Failed to decode pass1");

    let pass2_hex = digest(pass1_raw);
    let mut pass2_raw = decode(pass2_hex).expect("Failed to decode pass2");

    pass2_raw.reverse();

    println!("{}", encode(parse_u256_compact(get_difficulty())));

    encode(pass2_raw)
}

fn get_little_endian_string(input: i32) -> String {
    let input = input.to_le_bytes();
    encode(input)
}

fn get_pre_header() -> String {
    let version = get_version();
    let previous_block_hash =
        String::from("0000000000000000000000000000000000000000000000000000000000000000");
    let root_hash =
        String::from("3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a");
    let difficulty = get_difficulty();
    let time = get_unix_time();

    format!(
        "{}{}{}{}{}",
        version, previous_block_hash, root_hash, time, difficulty
    )
}

fn get_unix_time() -> String {
    // SystemTime::now().duration_since(UNIX_EPOCH).expect("Clock may have gone backwards")
    String::from("29ab5f49")
}

fn get_version() -> String {
    get_little_endian_string(1)
}

fn get_difficulty() -> String {
    let target = String::from("ffff001d"); // 0x1d00ffff -> big endian
    target
}

fn parse_u256_compact(compact_number: String) -> Vec<u8> {
    let mut vector = decode(compact_number).expect("Failed to decode compact number");
    if vector.len() != 4 {
        panic!("Invalid compact number length");
    }
    vector.reverse();
    let mut result = vec![0; vector[0] as usize];
    result[0] = vector[1];
    result[1] = vector[2];
    result[2] = vector[3];
    result
}
