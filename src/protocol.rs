use crate::byte_reader::ByteReader;
use crate::messages::message::MessagePayload::{PingMessage, PongMessage};
use crate::messages::message::{Message, MessagePayload, Payload};
use crate::messages::ping::Ping;
use crate::messages::pong::Pong;
use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

pub fn connect() -> Result<()> {
    let stream = TcpStream::connect("127.0.0.1:34352")?;

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
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

    let peer_addr = stream.peer_addr()?;
    println!("Handling connection from {}", peer_addr);
    let mut buffer = [0u8; 4096];
    let mut recv_buffer: Vec<u8> = Vec::new();

    let mut last_ping = Instant::now();

    loop {
        println!("Loop");
        match stream.read(&mut buffer) {
            Ok(0) => {
                println!("Connection with {peer_addr} closed");
                return Ok(());
            }
            Ok(n) => {
                println!("Received {} bytes", n);
                recv_buffer.extend(&buffer[0..n]);
                while let (Some(message), bytes_consumed) =
                    MessagePayload::try_parse_message(&mut recv_buffer)?
                {
                    recv_buffer.drain(0..bytes_consumed);

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

        if last_ping.elapsed() > Duration::from_secs(11) {
            let ping = Ping::new();
            let message = Message::new(ping);
            stream.write_all(&message.get_raw_format()?)?;
            last_ping = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
}
