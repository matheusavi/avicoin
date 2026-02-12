use crate::messages::message::Payload;
use crate::messages::ping::Ping;
use crate::util::command_12;
use anyhow::Result;

#[derive(Clone, Debug)]
pub struct Pong {
    pub nonce: u64,
}

impl Pong {
    pub fn new(ping: Ping) -> Result<Self> {
        Ok(Pong { nonce: ping.nonce })
    }
}

pub const PONG_COMMAND_NAME: &str = "pong";

impl Payload for Pong {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        Ok(Vec::from(&self.nonce.to_le_bytes()))
    }

    // TODO make this constant
    fn get_command_name(&self) -> [u8; 12] {
        command_12(PONG_COMMAND_NAME)
    }
}
