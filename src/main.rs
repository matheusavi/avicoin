use crate::block::Block;
use crate::protocol::frame_block;
use crate::transaction::{Outpoint, Transaction, TxIn, TxOut};
use hex::encode;

mod block;
mod block_storage;
mod protocol;
mod transaction;
mod util;
mod byte_reader;

fn main() {
    let mut block = Block::new(
        1,
        [0; 32],
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
    println!("The output is: {}", encode(block.hash.unwrap()));
    println!(
        "The serialized block is {}",
        encode(frame_block(block).unwrap())
    )
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
