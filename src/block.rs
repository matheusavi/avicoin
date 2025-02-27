pub struct Block {
    pub version: i32,
    pub previous_block_hash: String,
    pub merkle_root_hash: String,
    pub time: u32,
    pub difficulty: u32, // AKA nBits
    pub nonce: u32,
    pub hash: String,
    pub pre_hash: String
}

impl Block {
    pub fn new(version: i32, previous_block_hash: String, time: u32, difficulty: u32) -> Self {
        Block {
            version,
            previous_block_hash,
            merkle_root_hash: String::new(),
            time,
            difficulty,
            nonce: 0,
            hash: String::new(),
            pre_hash: String::new()
        }
    }
    
    pub fn calculate_pre_hash(&mut self){
        
    }
}
