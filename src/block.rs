use hex::decode;
use sha256::digest;
use std::f64;

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
    }

    fn get_hash(self) -> Vec<u8> {
        let pass1_hex = digest(&self.mine_array);
        let pass1_raw = decode(pass1_hex).expect("Failed to decode pass 1");

        let pass2_hex = digest(pass1_raw);
        let mut pass2_raw = decode(pass2_hex).expect("Failed to decode pass 2");

        pass2_raw.reverse();

        pass2_raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::decode;
    use primitive_types::U256;

    #[test]
    fn mine() {
        let mut block = Block {
            version: 1,
            previous_block_hash: String::from(
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            merkle_root_hash: String::from(
                "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
            ),
            time: 0x495fab29,
            n_bits: 0xffff001d,
            nonce: 0x7c2bac1d,
            hash: String::new(),
            mine_array: [0; 80],
        };

        block.prepare_for_mining();

        //I'm pretty sure this is wrong
        let hash = block.get_hash();

        let hash = U256::from_big_endian(&hash);
        let target: u32 = 0xffff001d;
        let exponent = target >> 24;
        let mantissa = target & 0x007FFFFF;
        let target = if exponent <= 3 {
            U256::from(mantissa >> (8 * (3 - exponent)))
        } else {
            let shift = 8 * (exponent - 3);
            if shift >= 256 {
                U256::max_value()
            } else {
                U256::from(mantissa) << shift
            }
        };
        assert!(hash < target);
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
