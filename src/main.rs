use crate::configs::get_configs;
use crate::protocol::{connect, listen};
use std::thread;

mod block;
mod block_storage;
mod byte_reader;
mod configs;
mod messages;
mod protocol;
mod transaction;
mod util;
mod wallet;

fn main() {
    let configs = get_configs();

    let host_address = configs.server.host_address;
    let addresses_to_connect = configs.server.addresses_to_connect;

    let handle = thread::spawn(|| {
        listen(host_address).unwrap();
    });

    for addr in addresses_to_connect {
        thread::spawn(|| connect(addr).unwrap());
    }

    handle.join().unwrap()
}
