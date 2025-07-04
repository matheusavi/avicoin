use crate::transaction::Transaction;
use crate::util::get_hash;
use hex::{decode, encode};
use primitive_types::U256;

pub struct Block {
    pub version: i32,
    pub previous_block_hash: String,
    pub merkle_root_hash: String,
    pub time: u32,
    pub n_bits: u32, // AKA difficulty
    pub nonce: u32,
    pub hash: String,
    mine_array: [u8; 80],
    transactions: Vec<Transaction>,
}

impl Block {
    pub fn new(
        version: i32,
        previous_block_hash: String,
        time: u32,
        n_bits: u32,
        transactions: Vec<Transaction>,
    ) -> Self {
        Block {
            version,
            previous_block_hash,
            merkle_root_hash: String::new(),
            time,
            n_bits,
            nonce: 0,
            hash: String::new(),
            mine_array: [0; 80],
            transactions,
        }
    }

    pub fn mine(&mut self) -> bool {
        self.merkle_root_hash = self.get_merkle_root_hash();
        self.prepare_for_mining();

        let n_bits = self.get_target_256();

        for nonce in 0..u32::MAX {
            self.mine_array[76..80].copy_from_slice(&nonce.to_le_bytes());
            let hash = get_hash(self.mine_array.as_slice());
            let hash256 = U256::from_big_endian(&hash);
            if hash256 < n_bits {
                self.nonce = nonce;
                self.hash = encode(&hash);
                return true;
            }
        }

        false
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

    fn get_target_256(&self) -> U256 {
        let target: u32 = self.n_bits;
        let exponent = target >> 24;
        let mantissa = target & 0x007FFFFF;

        let target = U256::from(mantissa);
        target << exponent * 8
    }

    fn get_merkle_root_hash(&self) -> String {
        let mut ids: Vec<[u8; 32]> = self.transactions.iter().map(|tx| tx.get_tx_id()).collect();

        if ids.len() == 0 {
            return String::from(
                "0000000000000000000000000000000000000000000000000000000000000000",
            );
        }
        if ids.len() == 1 {
            // the coinbase transaction will be here in the future and concatenated
            return encode(ids[0]);
        }

        while ids.len() > 1 {
            let mut count = ids.len();

            while count > 0 {
                let tx_id_1 = ids.pop().expect("Invalid tx_id array");
                count = count - 1;
                let tx_id_2 = match count {
                    0 => tx_id_1,
                    _ => ids.pop().expect("Invalid tx_id array"),
                };
                let concat = [&tx_id_1[..], &tx_id_2[..]].concat();
                let hash = get_hash(concat.as_slice());
                ids.push(hash.try_into().expect("Invalid hash"));
                if count > 0 {
                    count = count - 1;
                }
            }
        }
        encode(ids[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{Outpoint, TxIn, TxOut};
    use hex::{decode, encode};
    use primitive_types::U256;
    use rstest::rstest;

    #[rstest]
    #[case(1usize)]
    #[case(2usize)]
    #[case(3usize)]
    #[case(4usize)]
    fn mines_generates_correct_hash(#[case] number_of_transactions: usize) {
        let mut block = get_block(number_of_transactions);

        assert_eq!(block.mine(), true);
        assert_eq!(block.hash.len(), 64);
    }

    #[test]
    fn block_generates_correct_hash() {
        let mut block = get_block(2);

        block.prepare_for_mining();

        let hash = get_hash(block.mine_array.as_slice());

        let hash = U256::from_big_endian(&hash);
        let target = block.get_target_256();

        assert!(hash < target, "Hash should be lesser than target");

        assert_eq!(
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
            encode(hash.to_big_endian()),
            "Block hash is wrong"
        )
    }

    #[test]
    fn pre_hash_correctly_assembled() {
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
            transactions: vec![],
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

    fn get_tx() -> Transaction {
        Transaction {
            version: 1,
            inputs: {
                vec![TxIn {
                    previous_output: {
                        Outpoint {
                            tx_id: [0; 32],
                            v_out: 0,
                        }
                    },
                }]
            },
            outputs: vec![TxOut {
                value: 10_000,
                destiny_pub_key: "12345".to_string(),
            }],
            signature: "my_signature".to_string(),
        }
    }
    fn get_block(number_of_transactions: usize) -> Block {
        Block {
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
            transactions: vec![get_tx(); number_of_transactions],
        }
    }
}
