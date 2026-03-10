use crate::byte_reader::ByteReader;
use crate::messages::message::MessagePayload::{PingMessage, PongMessage};
use crate::messages::message::{Message, MessagePayload, Payload};
use crate::messages::pong::Pong;
use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const HEADER_LENGTH: usize = 24;
const MAGIC_BYTES: [u8; 4] = [0xf9, 0xbe, 0xb4, 0xd9];

pub fn send_message<T>(message: Message<T>) -> Result<()>
where
    T: Payload,
{
    let mut stream = TcpStream::connect("127.0.0.1:34352")?;

    stream.write_all(&message.get_raw_format()?)?;

    handle_connection(stream)?;

    Ok(())
}

pub fn listen() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:34352")?;

    for stream in listener.incoming() {
        handle_connection(stream?)?;
    }
    Ok(())
}

fn handle_messages(mut stream: &TcpStream, message: MessagePayload) -> Result<()> {
    match message {
        PingMessage(ping) => {
            println!("Ping received {:?}", ping);
            let pong = Pong::new(ping)?;
            let message = Message::new(pong);
            stream.write_all(&message.get_raw_format()?)?;
        }
        PongMessage(pong) => {
            println!("Pong received {:?}", pong)
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;

    let peer_addr = stream.peer_addr()?;
    println!("Handling connection from {}", peer_addr);
    let mut buffer = [0u8; 4096];
    let mut recv_buffer: Vec<u8> = Vec::new();

    loop {
        match stream.read(&mut buffer) {
            Ok(0) => {
                println!("Connection with {peer_addr} closed");
                return Ok(());
            }
            Ok(n) => {
                println!("Received {} bytes", n);
                recv_buffer.extend(&buffer[0..n]);
                while let Some(message) = try_parse_message(&mut recv_buffer)? {
                    handle_messages(&stream, message)?
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                {
                    println!("Connection timeout from {}", peer_addr);
                } else {
                    return Err(anyhow!("Read error: {}", e));
                }
            }
        }
    }
}

fn try_parse_message(recv_buffer: &mut Vec<u8>) -> Result<Option<MessagePayload>> {
    if recv_buffer.len() < HEADER_LENGTH {
        return Ok(None);
    }
    let mut reader = ByteReader::new(&recv_buffer);

    if reader.read_array::<4>()? != MAGIC_BYTES {
        return Err(anyhow!("Invalid magic bytes"));
    }

    let command_bytes = reader.read_array::<12>()?;
    let payload_size = reader.read_u32()? as usize;

    if recv_buffer.len() < (payload_size) + HEADER_LENGTH {
        return Ok(None);
    }
    let checksum = reader.read_array::<4>()?;
    let bytes = reader.read_bytes(payload_size)?;
    let message = MessagePayload::parse_raw(&command_bytes, bytes, checksum)?;

    recv_buffer.drain(0..HEADER_LENGTH + payload_size);

    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;
}
