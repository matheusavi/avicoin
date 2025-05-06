use crate::block::Block;
use crate::transaction::{Outpoint, Transaction, TxIn, TxOut};

mod block;
mod transaction;
mod util;

fn main() {
    let mut block = Block::new(
        1,
        String::from("0000000000000000000000000000000000000000000000000000000000000000"),
        0,
        0x1d00ffff,
        vec![
            get_tx(),
            get_tx(),
            get_tx(),
            get_tx(),
            get_tx(),
            get_tx(),
            get_tx(),
        ],
    );
    block.mine();
    println!("The output is: {}", block.hash);
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
// 1. Change txid to bytes
// 2. Create a merkle root with two transactions only
// 3. Transform the transactions vector into a tree
// 4. Handle cases with odd transactions
