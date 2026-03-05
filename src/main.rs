use crate::block::Block;
use crate::messages::message;
use crate::messages::message::Message;
use crate::messages::ping::Ping;
use crate::protocol::{frame_block, listen, send_block, send_message, unframe_block};
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

    let payload = Ping::new().expect("Failed to generate ping message");

    let message = Message::new(payload);

    let handle = thread::spawn(|| {
        listen().unwrap();
    });

    thread::spawn(|| send_message(message).unwrap());

    handle.join().unwrap()

    // todo, send/receive version message
    // todo, struct for version message
}
