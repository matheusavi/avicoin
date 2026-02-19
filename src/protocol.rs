use crate::block::Block;
use crate::messages::message::{Message, Payload};
use anyhow::{anyhow, Context, Result};
use hex::encode;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

const MAGIC_BYTES: [u8; 4] = [0xf9, 0xbe, 0xb4, 0xd9];
pub fn frame_block(block: Block) -> Result<Vec<u8>> {
    let mut bytes = block.get_raw_format()?;

    let length = bytes.len() as u32;

    bytes.splice(0..0, length.to_le_bytes());
    bytes.splice(0..0, MAGIC_BYTES);

    Ok(bytes)
}

pub fn unframe_block(bytes: Vec<u8>) -> Result<Block> {
    if bytes[0..4] != MAGIC_BYTES {
        return Err(anyhow!("Invalid magic bytes"));
    }
    let length = u32::from_le_bytes(bytes[4..8].try_into().context("Invalid length")?);
    Block::parse_raw(bytes[8..(length + 8) as usize].to_vec()).context("Failed to unframe block")
}

pub fn send_block(block: Block) -> Result<()> {
    let mut stream = TcpStream::connect("127.0.0.1:34352")?;

    stream.write(&frame_block(block)?)?;

    Ok(())
}

// connect -> sends version, verahack, responds to ping
pub fn send_message<T>(message: Message<T>) -> Result<()>
where
    T: Payload,
{
    let mut stream = TcpStream::connect("127.0.0.1:34352")?;

    stream.write(&message.get_raw_format()?)?;

    Ok(())
}

pub fn listen() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:34352")?;

    for stream in listener.incoming() {
        // I believe it's each connection
        seek_magic_bytes(&stream?)?;
        // handle_client(stream?)?;
    }
    Ok(())
}

fn handle_client(mut stream: TcpStream) -> Result<()> {
    let peer_addr = stream.peer_addr()?;
    println!("Handling connection from {}", peer_addr);

    stream.set_read_timeout(Some(Duration::from_secs(60)))?;

    
    
    let mut raw_format: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 1];
    
    
    // seek magic bytes
    // then read header
    // then read body
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => {
                println!("Connection closed");
                let mut block = unframe_block(raw_format)?;
                block.mine()?;
                println!("Received block with hash {}", encode(block.hash.unwrap()));
                break;
            }
            Ok(n) => {
                println!("Received {} bytes", n);
                raw_format.extend(&buffer);

                // not the best approach but it's a starting point
                if buffer[0] == MAGIC_BYTES[0] {
                    // read each next one
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                {
                    println!("Connection timeout from {}", peer_addr);
                    break;
                }
                return Err(anyhow!("Read error: {}", e));
            }
        }
    }

    Ok(())
}

fn seek_magic_bytes(stream: &TcpStream) -> Result<bool> {
    let mut raw_format: Vec<u8> = Vec::new();
    loop {
        // why does not need to be mut in child is mut?
        let byte = read_next_byte(&stream)?;
        raw_format.extend(&byte);

        match raw_format.len() {
            1_usize..=3_usize => {
                if raw_format != MAGIC_BYTES[0..raw_format.len()] {
                    raw_format.clear()
                }
            }
            4_usize => {
                if raw_format[0..4] == MAGIC_BYTES {
                    println!("Magic bytes found, reading message");
                    return Ok(true);
                } else {
                    raw_format.clear()
                }
            }
            _ => raw_format.clear(),
        }

        if raw_format.len() == 4 && raw_format[0..4] == MAGIC_BYTES {
            return Ok(true);
        }
        if raw_format != MAGIC_BYTES[0..raw_format.len()] {
            raw_format.clear()
        }
    }
}
fn read_next_byte(mut stream: &TcpStream) -> Result<[u8; 1]> {
    let mut buffer = [0u8; 1];
    match stream.read(&mut buffer) {
        Ok(0) => {
            println!("Connection closed");
            Err(anyhow!("Connection closed"))
        }
        Ok(n) => {
            println!("Received {} bytes", n);
            Ok(buffer)
        }
        Err(e) => Err(anyhow!("Read error: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::Block;
    use crate::transaction::{Outpoint, Transaction, TxIn, TxOut};

    fn dummy_block() -> Block {
        let mut block = Block::new(
            1,
            [0; 32],
            0,
            0x1d00ffff,
            vec![
                Transaction {
                    version: 1,
                    inputs: vec![TxIn {
                        previous_output: Outpoint {
                            tx_id: [0; 32],
                            v_out: 0,
                        },
                        signature: "my_signature".to_string(),
                        sequence: 0xFFFFFFFF,
                    }],
                    outputs: vec![TxOut {
                        value: 10_000,
                        destiny_pub_key: "12345".to_string(),
                    }],
                    lock_time: 0,
                },
                Transaction {
                    version: 1,
                    inputs: vec![TxIn {
                        previous_output: Outpoint {
                            tx_id: [0; 32],
                            v_out: 0,
                        },
                        signature: "my_signature".to_string(),
                        sequence: 0xFFFFFFFF,
                    }],
                    outputs: vec![TxOut {
                        value: 10_000,
                        destiny_pub_key: "12345".to_string(),
                    }],
                    lock_time: 0,
                },
            ],
        );
        block.mine().unwrap();
        block
    }

    #[test]
    fn test_frame_and_unframe_block() {
        let block = dummy_block();
        let framed = frame_block(block.clone()).expect("Should frame block");
        let unframed = unframe_block(framed).expect("Should unframe block");
        assert_eq!(unframed.version, block.version);
        assert_eq!(unframed.previous_block_hash, block.previous_block_hash);
        assert_eq!(unframed.nonce, block.nonce);
        assert_eq!(unframed.transactions.len(), block.transactions.len());
        assert_eq!(
            unframed.transactions[0].version,
            block.transactions[0].version
        );
    }

    #[test]
    fn test_unframe_invalid_magic_bytes() {
        let block = dummy_block();
        let mut framed = frame_block(block).unwrap();
        framed[0] = 0x00; // Corrupt magic bytes
        let result = unframe_block(framed);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Invalid magic bytes");
    }
}
