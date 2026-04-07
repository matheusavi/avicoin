use crate::byte_reader::ByteReader;
use crate::messages::message::Payload;
use crate::util::command_12;
use anyhow::Result;
use rand::Rng;

#[derive(Clone, Debug)]
pub struct Ping {
    pub nonce: u64,
}

pub const PING_COMMAND_NAME: &str = "ping";

impl Ping {
    pub fn new() -> Self {
        let mut rng = rand::rng();

        Ping {
            nonce: rng.next_u64(),
        }
    }

    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<Ping> {
        let mut reader = ByteReader::new(&bytes);
        Ok(Ping {
            nonce: reader.read_u64()?,
        })
    }
}

impl Payload for Ping {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        Ok(Vec::from(&self.nonce.to_le_bytes()))
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(PING_COMMAND_NAME)
    }
}
