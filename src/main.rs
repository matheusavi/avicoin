use crate::block::Block;
use crate::protocol::{frame_block, unframe_block};
use crate::wallet::Wallet;
use hex::encode;

mod block;
mod block_storage;
mod byte_reader;
mod protocol;
mod transaction;
mod util;
mod wallet;

fn main() {
    let wallet = Wallet::new();

    let destination_address = String::from("123jflsdhtyspei");

    let tx = wallet.send(1000, 10, destination_address);

    let mut block = Block::new(1, [0; 32], 0, 0x1d00ffff, vec![tx.unwrap()]);

    block.mine().unwrap();

    println!("The output is: {}", encode(block.hash.unwrap()));
}
