use crate::byte_reader::ByteReader;
use crate::messages::ping::{Ping, PING_COMMAND_NAME};
use crate::messages::pong::{Pong, PONG_COMMAND_NAME};
use crate::messages::verack::{Verack, VERACK_COMMAND_NAME};
use crate::messages::version::{Version, VERSION_COMMAND_NAME};
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
    VersionMessage(Message<Version>),
    VerackMessage,
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
        let mut reader = ByteReader::new(bytes);

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

        // Size before completeness, or an absurd claim reads as a message still arriving.
        if header.payload_size > MAX_PAYLOAD_SIZE {
            return Err(anyhow!("Payload too large: {}", header.payload_size));
        }

        if buffer.len() < HEADER_LENGTH + header.payload_size as usize {
            return Ok((None, 0));
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
            VERSION_COMMAND_NAME => MessageReceived::VersionMessage(Message {
                header,
                payload: Version::parse_raw_format(bytes)?,
            }),
            VERACK_COMMAND_NAME => {
                // Parsed for its refusal, not its value: a verack is the fact
                // of its arrival, and one carrying a body is not one of ours.
                Verack::parse_raw_format(bytes)?;
                MessageReceived::VerackMessage
            }
            _ => return Err(anyhow!("Unknown command: {}", command_name)),
        };

        Ok((Some(message), bytes_read))
    }
}

#[cfg(test)]
pub(crate) fn header_claiming(payload_size: u32) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(&MAGIC_BYTES);
    header.extend_from_slice(&crate::util::command_12(PING_COMMAND_NAME));
    header.extend_from_slice(&payload_size.to_le_bytes());
    header.extend_from_slice(&[0u8; 4]);
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn a_real_ping() -> (Vec<u8>, u64) {
        let ping = Ping::new();
        let nonce = ping.nonce;
        (Message::new(ping).unwrap().get_raw_format().unwrap(), nonce)
    }

    #[rstest]
    #[case::one_over(MAX_PAYLOAD_SIZE + 1)]
    #[case::four_gigabytes(u32::MAX)]
    fn an_oversized_payload_is_rejected_on_the_header_alone(#[case] claimed: u32) {
        let header = header_claiming(claimed);
        assert_eq!(HEADER_LENGTH, header.len());

        let error = MessageReceived::try_parse_message(&header)
            .expect_err("an oversized claim must be refused before its bytes are awaited");

        assert!(format!("{error:#}").contains("too large"), "got: {error:#}");
    }

    #[test]
    fn a_payload_at_the_limit_is_not_rejected_for_being_too_large() {
        let result = MessageReceived::try_parse_message(&header_claiming(MAX_PAYLOAD_SIZE));

        assert!(
            matches!(result, Ok((None, 0))),
            "a claim exactly at the limit is legal and merely incomplete, got: {result:?}"
        );
    }

    #[rstest]
    #[case::nothing(0)]
    #[case::half_a_header(HEADER_LENGTH / 2)]
    #[case::one_byte_short_of_a_header(HEADER_LENGTH - 1)]
    #[case::header_but_no_payload(HEADER_LENGTH)]
    #[case::header_and_half_a_payload(HEADER_LENGTH + 4)]
    fn an_incomplete_message_asks_for_more_bytes(#[case] available: usize) {
        let (message, _) = a_real_ping();

        let (parsed, consumed) = MessageReceived::try_parse_message(&message[..available])
            .expect("a partial message is not an error");

        assert!(parsed.is_none(), "{available} bytes should not parse");
        assert_eq!(
            0, consumed,
            "nothing may be consumed from a partial message"
        );
    }

    #[test]
    fn a_complete_message_parses_back_to_what_was_serialized() {
        let (message, nonce) = a_real_ping();

        let (parsed, consumed) = MessageReceived::try_parse_message(&message).unwrap();

        match parsed {
            Some(MessageReceived::PingMessage(ping)) => assert_eq!(nonce, ping.payload.nonce),
            other => panic!("expected a ping, got {other:?}"),
        }
        assert_eq!(message.len(), consumed);
    }

    #[test]
    fn trailing_bytes_of_a_second_message_are_left_alone() {
        let (mut buffer, _) = a_real_ping();
        let first_length = buffer.len();
        buffer.extend_from_slice(&a_real_ping().0);

        let (parsed, consumed) = MessageReceived::try_parse_message(&buffer).unwrap();

        assert!(parsed.is_some());
        assert_eq!(first_length, consumed, "only the first message is consumed");
    }

    #[test]
    fn foreign_magic_bytes_are_rejected() {
        let (mut message, _) = a_real_ping();
        message[0] ^= 0xff;

        MessageReceived::try_parse_message(&message)
            .expect_err("a message from another network must not be parsed");
    }

    #[test]
    fn a_corrupted_payload_fails_its_checksum() {
        let (mut message, _) = a_real_ping();
        let last = message.len() - 1;
        message[last] ^= 0xff;

        let error = MessageReceived::try_parse_message(&message)
            .expect_err("a payload that does not match its checksum must be rejected");

        assert!(format!("{error:#}").contains("checksum"), "got: {error:#}");
    }

    #[test]
    fn an_unknown_command_is_rejected() {
        let (mut message, _) = a_real_ping();
        message[4..16].copy_from_slice(&crate::util::command_12("notacommand"));

        MessageReceived::try_parse_message(&message)
            .expect_err("a command this node does not implement must be refused");
    }
}
