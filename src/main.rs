use crate::block::Block;

mod block;

fn main() {
    let mut block = Block::new(
        1,
        String::from("0000000000000000000000000000000000000000000000000000000000000000"),
        0,
        0x1d00ffff,
    );
    block.mine();
    println!("The output is: {}", block.hash);
}
