use crate::byte_reader::ByteReader;
use crate::messages::ping::{Ping, PING_COMMAND_NAME};
use crate::messages::pong::{Pong, PONG_COMMAND_NAME};
use crate::util::{get_hash, parse_command_12};
use anyhow::{anyhow, Result};

const MAGIC_BYTES: [u8; 4] = [0xf9, 0xbe, 0xb4, 0xd9];
const HEADER_LENGTH: usize = 24;
const MAX_PAYLOAD_SIZE: u32 = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Message<T> {
    header: Header,
    pub payload: T,
}

#[derive(Clone, Debug)]
pub struct Header {
    magic_bytes: [u8; 4],
    command_name: [u8; 12],
    payload_size: u32,
    checksum: [u8; 4],
}

pub trait Payload {
    fn get_raw_format(&self) -> Result<Vec<u8>>;
    fn get_command_name(&self) -> [u8; 12];
}

#[derive(Debug)]
pub enum MessageReceived {
    PingMessage(Message<Ping>),
    PongMessage(Message<Pong>),
}

impl Header {
    fn from_payload<T: Payload>(payload: &T) -> Result<Header> {
        let payload_bytes = payload.get_raw_format()?;
        let payload_size = payload_bytes.len() as u32;
        let payload_hash = get_hash(&payload_bytes);

        let checksum = *payload_hash
            .first_chunk::<4>()
            .expect("Invalid hashing array");

        Ok(Header {
            magic_bytes: MAGIC_BYTES,
            command_name: payload.get_command_name(),
            payload_size,
            checksum,
        })
    }

    fn get_raw_format(&self) -> [u8; HEADER_LENGTH] {
        let mut raw_format = [0; HEADER_LENGTH];

        raw_format[0..4].copy_from_slice(&self.magic_bytes);
        raw_format[4..16].copy_from_slice(&self.command_name);
        raw_format[16..20].copy_from_slice(&self.payload_size.to_le_bytes());
        raw_format[20..24].copy_from_slice(&self.checksum);

        raw_format
    }

    fn from_raw_format(bytes: &[u8]) -> Result<Header> {
        if bytes.len() < HEADER_LENGTH {
            return Err(anyhow!("Bytes smaller than header size"));
        }
        let mut reader = ByteReader::new(&bytes);

        let magic_bytes = reader.read_array::<4>()?;
        if magic_bytes != MAGIC_BYTES {
            return Err(anyhow!("Invalid magic bytes"));
        }

        let command_name = reader.read_array::<12>()?;
        let payload_size = reader.read_u32()?;

        let checksum = reader.read_array::<4>()?;

        Ok(Header {
            magic_bytes,
            command_name,
            payload_size,
            checksum,
        })
    }
}

impl<T> Message<T>
where
    T: Payload,
{
    pub fn new(payload: T) -> Result<Message<T>> {
        Ok(Message {
            header: Header::from_payload(&payload)?,
            payload,
        })
    }

    pub fn get_raw_format(&self) -> Result<Vec<u8>> {
        let mut raw_format = Vec::new();

        raw_format.extend_from_slice(&self.header.get_raw_format());
        raw_format.extend_from_slice(&self.payload.get_raw_format()?);

        Ok(raw_format)
    }
}

impl MessageReceived {
    pub(crate) fn try_parse_message(buffer: &[u8]) -> Result<(Option<MessageReceived>, usize)> {
        if buffer.len() < HEADER_LENGTH {
            return Ok((None, 0));
        }

        let header = Header::from_raw_format(&buffer[..HEADER_LENGTH])?;

        if buffer.len() < HEADER_LENGTH + header.payload_size as usize {
            // It's possible that we partially read the input
            return Ok((None, 0));
        }

        if header.payload_size > MAX_PAYLOAD_SIZE {
            return Err(anyhow!("Payload too large: {}", header.payload_size));
        }

        let mut reader =
            ByteReader::new(&buffer[HEADER_LENGTH..header.payload_size as usize + HEADER_LENGTH]);

        let bytes = reader.read_bytes(header.payload_size as usize)?;

        let hash = get_hash(&bytes);
        let generated_checksum = hash.first_chunk::<4>().expect("Invalid hashing array");

        if header.checksum != *generated_checksum {
            return Err(anyhow!("Invalid checksum"));
        }

        let command_name = parse_command_12(&header.command_name)?;

        let bytes_read = HEADER_LENGTH + header.payload_size as usize;

        let message = match command_name {
            PING_COMMAND_NAME => MessageReceived::PingMessage(Message {
                header,
                payload: Ping::parse_raw_format(bytes)?,
            }),
            PONG_COMMAND_NAME => MessageReceived::PongMessage(Message {
                header,
                payload: Pong::parse_raw_format(bytes)?,
            }),
            _ => return Err(anyhow!("Unknown command: {}", command_name)),
        };

        Ok((Some(message), bytes_read))
    }
}
