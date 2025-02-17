use crate::miner::mine;

mod miner;

fn main() {
    let expected = String::from("000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f");
    println!("The output is: {}", mine() == expected);
}
