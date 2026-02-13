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

#[cfg(test)]
mod tests {
    use hex::encode;
    use super::*;

    #[test]
    fn get_hash_empty_input() {
        let result = get_hash(&decode("0000ff3f782d956c8278430d554c38b24a3fa5cf1e57ad5265561c000000000000000000fdbcdd0e7216037429d7d3d6c89520ba23b16b78675f1d6563bb3bc4497964a662e0085c7cd93117c2a95df9").unwrap()[..]);
        println!("The output is: {}", encode(result));
        assert_eq!(result.len(), 32);
        // this seems to be incorrect
        // TODO: check it, I believe it's used in internal byte order almost exclusively 
    }

    #[test]
    fn get_hash_known_input() {
        let input = b"hello";
        let result = get_hash(input);
        assert_eq!(result.len(), 32);
        let result2 = get_hash(input);
        assert_eq!(result, result2);
    }

    #[test]
    fn get_hash_different_inputs_different_outputs() {
        let result1 = get_hash(b"hello");
        let result2 = get_hash(b"world");
        assert_ne!(result1, result2);
    }

    #[test]
    fn get_compact_int_single_byte_zero() {
        let result = get_compact_int(0);
        assert_eq!(result, vec![0x00]);
    }

    #[test]
    fn get_compact_int_single_byte_max() {
        let result = get_compact_int(252);
        assert_eq!(result, vec![0xfc]);
    }

    #[test]
    fn get_compact_int_two_byte_min() {
        let result = get_compact_int(253);
        assert_eq!(result, vec![0xfd, 0xfd, 0x00]);
    }

    #[test]
    fn get_compact_int_two_byte_max() {
        let result = get_compact_int(0xffff);
        assert_eq!(result, vec![0xfd, 0xff, 0xff]);
    }

    #[test]
    fn get_compact_int_four_byte_min() {
        let result = get_compact_int(0x10000);
        assert_eq!(result, vec![0xfe, 0x00, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn get_compact_int_four_byte_max() {
        let result = get_compact_int(0xffff_ffff);
        assert_eq!(result, vec![0xfe, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn get_compact_int_eight_byte_min() {
        let result = get_compact_int(0x1_0000_0000);
        assert_eq!(
            result,
            vec![0xff, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn get_compact_int_eight_byte_large() {
        let result = get_compact_int(u64::MAX);
        assert_eq!(
            result,
            vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
    }

    #[test]
    fn command_12_short_command() {
        let result = command_12("ping");
        assert_eq!(result, *b"ping\0\0\0\0\0\0\0\0");
    }

    #[test]
    fn command_12_max_length() {
        let result = command_12("123456789012");
        assert_eq!(result, *b"123456789012");
    }

    #[test]
    fn command_12_empty_string() {
        let result = command_12("");
        assert_eq!(result, [0u8; 12]);
    }

    #[test]
    #[should_panic(expected = "command too long")]
    fn command_12_too_long() {
        command_12("1234567890123");
    }

    #[test]
    #[should_panic(expected = "command must be ASCII")]
    fn command_12_non_ascii() {
        command_12("ping🚀");
    }

    #[test]
    fn serialize_then_parse_command_12() {
        let bytes = command_12("ping");

        let response = parse_command_12(&bytes);

        assert_eq!(bytes, *b"ping\0\0\0\0\0\0\0\0");
        assert_eq!(response.unwrap(), "ping");
    }

    #[test]
    fn parse_command_12_full_length() {
        let bytes: [u8; 12] = *b"123456789012";
        let result = parse_command_12(&bytes);
        assert_eq!(result.unwrap(), "123456789012");
    }

    #[test]
    fn parse_command_12_empty() {
        let bytes: [u8; 12] = [0u8; 12];
        let result = parse_command_12(&bytes);
        assert_eq!(result.unwrap(), "");
    }

    #[test]
    fn parse_command_12_invalid_padding() {
        let bytes: [u8; 12] = *b"ping\0test\0\0\0";
        let result = parse_command_12(&bytes);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid command padding"));
    }

    #[test]
    fn parse_command_12_valid_padding() {
        let bytes: [u8; 12] = *b"version\0\0\0\0\0";
        let result = parse_command_12(&bytes);
        assert_eq!(result.unwrap(), "version");
    }
}
