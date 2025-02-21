use hex::{decode, encode};
use sha256::digest;
// use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn mine() -> String {
    let mut nonce: u32 = 0;
    // let mut step: u32 = u32::from_le_bytes([0x1d, 0xac, 0x2b, 0x7c]); // this is the nonce for the current block
    let target = parse_u256_compact(get_difficulty());
    while nonce < u32::MAX {
        let encoded_bytes = encode(nonce.to_le_bytes());
        let hash = get_hash(&encoded_bytes);
        if is_smaller(&hash, &target) {
            return encode(hash);
        }
        nonce = nonce + 1;
    }
    "".to_string()
}

fn is_smaller(hash: &Vec<u8>, target: &Vec<u8>) -> bool {
    let hash_first_digit = get_first_significant_digit(hash);
    let hash_length = hash.len() - hash_first_digit;
    let target_first_digit = get_first_significant_digit(target);
    let target_length = target.len() - target_first_digit;
    if hash_length < target_length {
        return true;
    } else if hash_length > target_length {
        return false;
    }

    if hash[hash_first_digit] < target[target_first_digit] {
        return true;
    } else if hash[hash_first_digit] > target[target_first_digit] {
        return false;
    }

    if hash[hash_first_digit + 1] < target[target_first_digit + 1] {
        return true;
    } else if hash[hash_first_digit + 1] > target[target_first_digit + 1] {
        return false;
    }

    if hash[hash_first_digit + 2] < target[target_first_digit + 2] {
        return true;
    } else if hash[hash_first_digit + 2] > target[target_first_digit + 2] {
        return false;
    }

    false
}

fn get_first_significant_digit(vector: &Vec<u8>) -> usize {
    let mut index = 0;
    for element in vector.iter() {
        if *element != 0 {
            return index;
        }
        index = index + 1;
    }
    vector.len()
}

fn get_hash(nonce: &String) -> Vec<u8> {
    let block_header = get_pre_header() + &nonce;
    let hex_block_header = decode(&block_header).expect("Failed to decode block header");

    let pass1_hex = digest(hex_block_header);
    let pass1_raw = decode(pass1_hex).expect("Failed to decode pass1");

    let pass2_hex = digest(pass1_raw);
    let mut pass2_raw = decode(pass2_hex).expect("Failed to decode pass2");

    pass2_raw.reverse();

    pass2_raw
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

/// Parse little endian compact number to a big endian hex array
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
