use crate::block::Block;
use crate::protocol::{connect, listen};
use crate::wallet::Wallet;
use hex::encode;
use std::thread;

mod block;
mod block_storage;
mod byte_reader;
mod messages;
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

    let handle = thread::spawn(|| {
        listen("127.0.0.1:34352").unwrap();
    });

    thread::spawn(|| connect("127.0.0.1:34352").unwrap());

    handle.join().unwrap()
}
