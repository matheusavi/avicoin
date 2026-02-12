use anyhow::{anyhow, Context, Result};
use hex::decode;
use sha256::digest;

pub fn get_hash(slice: &[u8]) -> [u8; 32] {
    let pass1_hex = digest(slice);
    let pass1_raw = decode(pass1_hex).expect("Failed to decode pass 1");

    let pass2_hex = digest(pass1_raw);
    let mut pass2_raw = decode(pass2_hex).expect("Failed to decode pass 2");

    pass2_raw.reverse();

    let pass2_raw = pass2_raw
        .try_into()
        .expect("Failed to convert pass2 to array");

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

pub fn command_12(cmd: &str) -> [u8; 12] {
    let bytes = cmd.as_bytes();
    assert!(bytes.len() <= 12, "command too long");

    assert!(bytes.is_ascii(), "command must be ASCII");

    let mut out = [0u8; 12];
    out[..bytes.len()].copy_from_slice(bytes);
    out
}

pub fn parse_command_12(cmd_bytes: &[u8; 12]) -> Result<&str> {
    let mut iterator = cmd_bytes.iter();
    let first_null = iterator.position(|&b| b == 0);

    let cleaned_bytes = match first_null {
        Some(position) => {
            let not_zero_byte = iterator.position(|&b| b != 0);
            if not_zero_byte.is_some() {
                return Err(anyhow!("Invalid command padding"));
            }
            &cmd_bytes[..position]
        }
        _ => &cmd_bytes[..],
    };

    if !cleaned_bytes.is_ascii() {
        return Err(anyhow!("Invalid command padding"));
    }

    std::str::from_utf8(cleaned_bytes).context("Failed to parse utf8 in command string")
}
