use crate::util::get_hash;
use anyhow::{anyhow, Context, Result};
use std::ptr::hash;

#[derive(Clone, Debug)]
pub struct Message<T> {
    payload: T,
}

// Should this be a separate file?
pub trait Payload {
    fn get_raw_format(&self) -> Result<Vec<u8>>;
    // I think we should use string constant to make it easier
    fn get_command_name(&self) -> [u8; 12];
}

// do it at the brute force then optimize
// assemble pong
// serialize
// header should be based on Pong data
// so you can instead create it based on pong data ready to send
// send
// receive
// use header to know how many bytes read
// type of message to deserialize
// checksum the output

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

    pub fn parse_raw(command_name: [u8; 12], bytes: Vec<u8>) -> Result<Message<T>> {
        // match for command name and parse
        
    }
}
