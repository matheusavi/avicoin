use crate::byte_reader::ByteReader;
use crate::messages::message::Payload;
use crate::messages::ping::Ping;
use crate::util::command_12;
use anyhow::Result;

#[derive(Clone, Debug)]
pub struct Pong {
    pub nonce: u64,
}

pub const PONG_COMMAND_NAME: &str = "pong";

impl Pong {
    pub fn new(ping: Ping) -> Result<Self> {
        Ok(Pong { nonce: ping.nonce })
    }
    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<Pong> {
        let mut reader = ByteReader::new(&bytes);
        Ok(Pong {
            nonce: reader.read_u64()?,
        })
    }
}

impl Payload for Pong {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        Ok(Vec::from(&self.nonce.to_le_bytes()))
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(PONG_COMMAND_NAME)
    }
}
