use crate::messages::ping::{Ping, PING_COMMAND_NAME};
use crate::messages::pong::{Pong, PONG_COMMAND_NAME};
use crate::util::{get_hash, parse_command_12};
use anyhow::{anyhow, Result};

const MAGIC_BYTES: [u8; 4] = [0xf9, 0xbe, 0xb4, 0xd9];

#[derive(Clone, Debug)]
pub struct Message<T> {
    payload: T,
}

// Should this be a separate file?
pub trait Payload {
    fn get_raw_format(&self) -> Result<Vec<u8>>;
    fn get_command_name(&self) -> [u8; 12];
}

pub enum MessagePayload {
    Ping(Ping),
    Pong(Pong),
}

impl<T> Message<T>
where
    T: Payload,
{
    pub fn new(payload: T) -> Message<T> {
        Message { payload }
    }

    pub fn get_raw_format(&self) -> Result<Vec<u8>> {
        let mut raw_format = Vec::new();

        let payload_bytes = self.payload.get_raw_format()?;
        let payload_hash = get_hash(&payload_bytes);

        raw_format.extend(MAGIC_BYTES);
        raw_format.extend(&self.payload.get_command_name());
        raw_format.extend((payload_bytes.len() as u32).to_le_bytes());

        // checksum
        raw_format.extend(
            *payload_hash
                .first_chunk::<4>()
                .expect("Invalid hashing array"),
        );

        raw_format.extend_from_slice(&payload_bytes);

        Ok(raw_format)
    }
}

pub fn parse_raw(command_name: &[u8; 12], bytes: Vec<u8>) -> Result<MessagePayload> {
    let command_name = parse_command_12(command_name)?;

    match command_name {
        PING_COMMAND_NAME => Ok(MessagePayload::Ping(Ping::parse_raw_format(bytes)?)),
        PONG_COMMAND_NAME => Ok(MessagePayload::Pong(Pong::parse_raw_format(bytes)?)),
        _ => Err(anyhow!("Not implemented")),
    }
}
