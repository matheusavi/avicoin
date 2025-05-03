use crate::block::Block;

mod block;
mod transaction;
mod util;

fn main() {
    let mut block = Block::new(
        1,
        String::from("0000000000000000000000000000000000000000000000000000000000000000"),
        0,
        0x1d00ffff,
        vec![],
    );
    block.mine();
    println!("The output is: {}", block.hash);
}

// 1. Change txid to bytes
// 2. Create a merkle root with two transactions only
// 3. Transform the transactions vector into a tree
// 4. Handle cases with odd transactions
