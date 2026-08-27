use crate::crypto::{PubKeyHash, PublicKey, PUBKEY_HASH_LEN};
use crate::util::{get_hash, hash160};
use anyhow::{anyhow, Result};
use std::fmt;
use std::str::FromStr;

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const CHECKSUM_LEN: usize = 4;

/// Avi Coin's version byte. Non-zero, so no payload ever starts with a zero
/// byte and every address is 34 characters beginning with `A` — ADR-0005.
pub const VERSION: u8 = 0x17;

/// A public key hash in the form a human can carry. Holds the hash, not the
/// text, so an `Address` that exists is one that encodes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Address(PubKeyHash);

impl Address {
    pub fn for_pubkey_hash(hash: PubKeyHash) -> Self {
        Address(hash)
    }

    pub fn for_public_key(key: &PublicKey) -> Self {
        Address(PubKeyHash::from_bytes(hash160(key.as_bytes())))
    }

    pub fn pubkey_hash(&self) -> PubKeyHash {
        self.0
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut payload = vec![VERSION];
        payload.extend(self.0.as_bytes());
        payload.extend(&get_hash(&payload)[..CHECKSUM_LEN]);

        write!(f, "{}", encode_base58(&payload))
    }
}

impl FromStr for Address {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Address> {
        let decoded = decode_base58(text)?;

        if decoded.len() != 1 + PUBKEY_HASH_LEN + CHECKSUM_LEN {
            return Err(anyhow!(
                "an address decodes to {} bytes, got {}",
                1 + PUBKEY_HASH_LEN + CHECKSUM_LEN,
                decoded.len()
            ));
        }

        let (payload, checksum) = decoded.split_at(decoded.len() - CHECKSUM_LEN);
        if checksum != &get_hash(payload)[..CHECKSUM_LEN] {
            return Err(anyhow!("checksum does not match: {text} is mistyped"));
        }

        let (version, hash) = payload.split_first().expect("payload is never empty");
        if *version != VERSION {
            return Err(anyhow!(
                "version byte {version:#04x} is not Avi Coin's {VERSION:#04x}"
            ));
        }

        Ok(Address(PubKeyHash::from_bytes(
            hash.try_into().expect("length was checked above"),
        )))
    }
}

fn encode_base58(bytes: &[u8]) -> String {
    let mut digits: Vec<u8> = Vec::new();

    for &byte in bytes {
        let mut carry = byte as u32;
        for digit in digits.iter_mut() {
            carry += (*digit as u32) << 8;
            *digit = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }

    // Base 58 has no way to carry a leading zero byte, so each one is written
    // out separately. Unreachable at Avi Coin's version byte; correct anyway.
    let leading_zeros = bytes.iter().take_while(|&&byte| byte == 0).count();

    "1".repeat(leading_zeros)
        .into_bytes()
        .into_iter()
        .chain(digits.iter().rev().map(|&digit| ALPHABET[digit as usize]))
        .map(char::from)
        .collect()
}

fn decode_base58(text: &str) -> Result<Vec<u8>> {
    let mut bytes: Vec<u8> = Vec::new();

    for character in text.chars() {
        let value = ALPHABET
            .iter()
            .position(|&candidate| candidate == character as u8 && character.is_ascii())
            .ok_or_else(|| anyhow!("{character:?} is not a base58 character"))?;

        let mut carry = value as u32;
        for byte in bytes.iter_mut() {
            carry += (*byte as u32) * 58;
            *byte = carry as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push(carry as u8);
            carry >>= 8;
        }
    }

    let leading_ones = text
        .chars()
        .take_while(|&character| character == '1')
        .count();

    Ok(std::iter::repeat_n(0, leading_ones)
        .chain(bytes.into_iter().rev())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::PrivateKey;
    use rstest::rstest;

    fn encode_check(version: u8, hash: &str) -> String {
        let mut payload = vec![version];
        payload.extend(hex::decode(hash).unwrap());
        payload.extend(&get_hash(&payload)[..CHECKSUM_LEN]);

        encode_base58(&payload)
    }

    /// Public Bitcoin vectors, so they are answers this code cannot have
    /// invented. Version `0x00` also exercises the leading-zero rule, which
    /// Avi Coin's own version byte makes unreachable.
    #[rstest]
    #[case(
        "0000000000000000000000000000000000000000",
        "1111111111111111111114oLvT2"
    )]
    #[case(
        "62e907b15cbf27d5425399ebf6f0fb50ebb88f18",
        "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"
    )]
    #[case(
        "010966776006953d5567439e5e39f86a0d273bee",
        "16UwLL9Risc3QfPqBUvKofHmBQ7wMtjvM"
    )]
    fn base58check_matches_published_bitcoin_addresses(#[case] hash: &str, #[case] expected: &str) {
        assert_eq!(encode_check(0x00, hash), expected);
    }

    #[rstest]
    #[case("0000000000000000000000000000000000000000")]
    #[case("62e907b15cbf27d5425399ebf6f0fb50ebb88f18")]
    #[case("ffffffffffffffffffffffffffffffffffffffff")]
    fn an_avi_coin_address_is_thirty_four_characters_beginning_with_a(#[case] hash: &str) {
        let address = Address::for_pubkey_hash(PubKeyHash::from_bytes(
            hex::decode(hash).unwrap().try_into().unwrap(),
        ))
        .to_string();

        assert_eq!(address.len(), 34);
        assert!(address.starts_with('A'), "{address}");
    }

    #[test]
    fn an_address_round_trips_through_its_text() {
        let hash = PubKeyHash::from_bytes([0x2a; PUBKEY_HASH_LEN]);
        let address = Address::for_pubkey_hash(hash);

        assert_eq!(address.to_string().parse::<Address>().unwrap(), address);
        assert_eq!(
            address
                .to_string()
                .parse::<Address>()
                .unwrap()
                .pubkey_hash(),
            hash
        );
    }

    #[test]
    fn a_public_key_reaches_its_address_through_hash160() {
        let key = PrivateKey::random().public_key();
        let address = Address::for_public_key(&key);

        assert_eq!(
            address.pubkey_hash().as_bytes(),
            &hash160(key.as_bytes()),
            "an address commits to the hash of the key, never to the key"
        );
    }

    #[test]
    fn a_mistyped_character_is_caught_by_the_checksum() {
        let address =
            Address::for_pubkey_hash(PubKeyHash::from_bytes([1; PUBKEY_HASH_LEN])).to_string();
        let mistyped: String = address
            .char_indices()
            .map(|(index, character)| if index == 9 { 'Z' } else { character })
            .collect();

        assert_ne!(mistyped, address);
        assert!(mistyped.parse::<Address>().is_err());
    }

    #[test]
    fn another_networks_version_byte_is_refused() {
        let bitcoin = encode_check(0x00, "62e907b15cbf27d5425399ebf6f0fb50ebb88f18");

        let refusal = bitcoin.parse::<Address>().unwrap_err().to_string();

        assert!(refusal.contains("0x00"), "{refusal}");
    }

    #[rstest]
    #[case::empty("")]
    #[case::not_base58("A0OIl111111111111111111111111111111")]
    #[case::too_short("A1zP1eP5QGefi2DMPTfTL5SLmv7Divf")]
    #[case::too_long("AFmseVrdL9f9oyCzZefL9tG6UbvhPbdYzMxx")]
    fn text_that_is_not_an_address_is_refused(#[case] text: &str) {
        assert!(text.parse::<Address>().is_err());
    }
}
