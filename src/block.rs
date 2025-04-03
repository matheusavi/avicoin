use hex::{decode, encode};
use primitive_types::U256;
use sha256::digest;

pub struct Block {
    pub version: i32, // not sure if I should already store this as little endian constants :crazy:
    pub previous_block_hash: String,
    pub merkle_root_hash: String,
    pub time: u32,
    pub n_bits: u32, // AKA difficulty
    pub nonce: u32,
    pub hash: String,
    mine_array: [u8; 80],
}

impl Block {
    pub fn new(version: i32, previous_block_hash: String, time: u32, n_bits: u32) -> Self {
        Block {
            version,
            previous_block_hash,
            merkle_root_hash: String::new(),
            time,
            n_bits,
            nonce: 0,
            hash: String::new(),
            mine_array: [0; 80],
        }
    }

    fn prepare_for_mining(&mut self) {
        self.mine_array[0..4].copy_from_slice(&self.version.to_le_bytes());

        let previous_block_hash =
            decode(&self.previous_block_hash).expect("Invalid previous block hash");

        self.mine_array[4..36].copy_from_slice(&previous_block_hash);

        let merkle_root_hash = decode(&self.merkle_root_hash).expect("Invalid merkle root hash");
        self.mine_array[36..68].copy_from_slice(&merkle_root_hash);

        self.mine_array[68..72].copy_from_slice(&self.time.to_le_bytes());

        self.mine_array[72..76].copy_from_slice(&self.n_bits.to_le_bytes());

        self.mine_array[76..80].copy_from_slice(&self.nonce.to_le_bytes());
    }

    fn get_hash(&self) -> Vec<u8> {
        let pass1_hex = digest(&self.mine_array);
        let pass1_raw = decode(pass1_hex).expect("Failed to decode pass 1");

        let pass2_hex = digest(pass1_raw);
        let mut pass2_raw = decode(pass2_hex).expect("Failed to decode pass 2");

        pass2_raw.reverse();

        pass2_raw
    }

    fn mine(&mut self) -> bool {
        self.prepare_for_mining();

        let n_bits = self.get_target_256();

        for nonce in 0..u32::MAX {
            self.mine_array[76..80].copy_from_slice(&nonce.to_le_bytes());
            let hash = self.get_hash();
            let hash = U256::from_big_endian(&hash);
            if hash < n_bits {
                self.hash = encode(&hash.to_big_endian());
                return true;
            }
        }

        false
    }

    fn get_target_256(&self) -> U256 {
        let target: u32 = self.n_bits;
        let exponent = target >> 24;
        let mantissa = target & 0x007FFFFF;

        let target = U256::from(mantissa);
        target << exponent * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::{decode, encode};
    use primitive_types::U256;

    #[test]
    fn test_mining() {
        let mut block = Block {
            version: 1,
            previous_block_hash: String::from(
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            merkle_root_hash: String::from(
                "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
            ),
            time: 0x495fab29,
            n_bits: 0x1d00ffff,
            nonce: 0x7c2bac1d,
            hash: String::new(),
            mine_array: [0; 80],
        };

        assert_eq!(block.mine(), true);
        println!("{}", block.hash);
        println!("{}", block.get_target_256())
    }

    #[test]
    fn validate_hash() {
        let mut block = Block {
            version: 1,
            previous_block_hash: String::from(
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            merkle_root_hash: String::from(
                "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
            ),
            time: 0x495fab29,
            n_bits: 0x1d00ffff,
            nonce: 0x7c2bac1d,
            hash: String::new(),
            mine_array: [0; 80],
        };

        block.prepare_for_mining();

        let hash = block.get_hash();

        let hash = U256::from_big_endian(&hash);
        let target: u32 = 0x1d00ffff;
        let exponent = target >> 24;
        let mantissa = target & 0x007FFFFF;

        let target = U256::from(mantissa);
        let target = target << exponent * 8;

        assert!(hash < target, "Hash should be lesser than target");

        assert_eq!(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
            encode(hash.to_big_endian()),
            "Block hash is wrong"
        )
    }

    #[test]
    fn test_pre_hash() {
        let mut block = Block {
            version: 1,
            previous_block_hash: String::from(
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            merkle_root_hash: String::from(
                "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
            ),
            time: 0x495fab29,
            n_bits: 0x1d00ffff,
            nonce: 0,
            hash: String::new(),
            mine_array: [0; 80],
        };

        block.prepare_for_mining();

        let expected_previous_block_hash =
            decode("0000000000000000000000000000000000000000000000000000000000000000").unwrap();
        let expected_merkle_root_hash =
            decode("3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a").unwrap();

        assert_eq!(
            block.mine_array.len(),
            80,
            "mine_array length does not match expected length"
        );

        assert_eq!(
            &block.mine_array[0..4],
            0x01000000u32.to_be_bytes(),
            "Version part does not match"
        );

        assert_eq!(
            &block.mine_array[4..36],
            expected_previous_block_hash,
            "Previous block hash part does not match"
        );

        assert_eq!(
            &block.mine_array[36..68],
            expected_merkle_root_hash,
            "Merkle root hash part does not match"
        );

        assert_eq!(
            &block.mine_array[68..72],
            0x29ab5f49u32.to_be_bytes(),
            "Time part does not match"
        );

        assert_eq!(
            &block.mine_array[72..76],
            0xffff001du32.to_be_bytes(),
            "n_bits part does not match"
        );
    }
}
