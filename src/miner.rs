use hex::{decode, encode};
use sha256::digest;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn mine() -> String {
    let nonce = "1dac2b7c";
    let block_header = get_pre_header() + nonce;
    let hex_block_header = decode(&block_header).expect("Failed to decode block header");

    let pass1_hex = digest(hex_block_header);
    let pass1_raw = decode(pass1_hex).expect("Failed to decode pass1");

    let pass2_hex = digest(pass1_raw);
    let mut pass2_raw = decode(pass2_hex).expect("Failed to decode pass2");

    pass2_raw.reverse();

    encode(&pass2_raw)
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
    // let version = 2;
    String::from("01000000")
}

fn get_difficulty() -> String {
    String::from("ffff001d")
}
