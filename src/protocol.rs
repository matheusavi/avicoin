use crate::messages::message::MessageReceived::{PingMessage, PongMessage};
use crate::messages::message::{Message, MessageReceived};
use crate::messages::ping::Ping;
use crate::messages::pong::Pong;
use anyhow::{anyhow, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

const PING_INTERVAL: Duration = Duration::from_secs(11);

/// Dials a peer and runs the connection on its own thread.
pub fn connect(addr: SocketAddr) -> Result<()> {
    let stream = TcpStream::connect(addr)?;
    spawn_connection(stream);

    Ok(())
}

/// Accepts connections until the listener fails.
///
/// Each connection is handled on its own thread: `handle_connection` blocks
/// until the peer disconnects, so handling one inline would mean the node could
/// only ever hold a single peer. One peer's failure is logged rather than
/// propagated, so a misbehaving peer cannot take the listener down with it.
pub fn listen(listener: TcpListener) -> Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => spawn_connection(stream),
            Err(e) => println!("Could not accept a connection: {e}"),
        }
    }

    Ok(())
}

/// Whether a ping is due at `now`. The first one is due immediately — `None`
/// means none has been sent yet — and later ones once `interval` has elapsed.
///
/// `now` is a parameter rather than read inside so the boundary can be tested
/// exactly, without sleeping and without an interval so small the assertion
/// would hold whatever the inputs.
fn ping_is_due(last_ping: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last_ping.is_none_or(|sent| now.saturating_duration_since(sent) >= interval)
}

/// Runs a connection on its own thread, reporting how it ended.
///
/// The one place a connection becomes a thread, whether it was dialled or
/// accepted — so M1's peer registry (issue #16) registers and unregisters here
/// rather than at two call sites that would drift.
fn spawn_connection(stream: TcpStream) {
    let peer = stream
        .peer_addr()
        .map_or_else(|_| "an unknown peer".to_string(), |addr| addr.to_string());

    thread::spawn(move || {
        if let Err(e) = handle_connection(stream) {
            println!("Connection with {peer} ended: {e:#}");
        }
    });
}

fn handle_connection(mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

    let peer_addr = stream.peer_addr()?;
    println!("Handling connection from {}", peer_addr);
    let mut buffer = [0u8; 4096];
    let mut recv_buffer: Vec<u8> = Vec::new();

    let mut last_ping: Option<Instant> = None;

    loop {
        if ping_is_due(last_ping, Instant::now(), PING_INTERVAL) {
            let ping = Ping::new();
            let message = Message::new(ping)?;
            stream.write_all(&message.get_raw_format()?)?;
            last_ping = Some(Instant::now());
        }

        match stream.read(&mut buffer) {
            Ok(0) => {
                println!("Connection with {peer_addr} closed");
                return Ok(());
            }
            Ok(n) => {
                println!("Received {} bytes", n);
                process_incoming_bytes(&mut stream, &mut recv_buffer, &buffer[..n])?
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

fn process_incoming_bytes<W: Write>(
    writer: &mut W,
    recv_buffer: &mut Vec<u8>,
    buffer: &[u8],
) -> Result<()> {
    recv_buffer.extend(buffer);
    while let (Some(message), bytes_consumed) = MessageReceived::try_parse_message(recv_buffer)? {
        recv_buffer.drain(0..bytes_consumed);

        handle_messages(writer, message)?
    }
    Ok(())
}

fn handle_messages<W: Write>(writer: &mut W, message: MessageReceived) -> Result<()> {
    match message {
        PingMessage(ping) => {
            println!("Ping received {:?}", ping);
            let pong = Pong::new(ping.payload)?;
            let message = Message::new(pong)?;
            writer.write_all(&message.get_raw_format()?)?;
        }
        PongMessage(pong) => {
            println!("Pong received {:?}", pong)
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn the_first_ping_is_due_immediately() {
        assert!(
            ping_is_due(None, Instant::now(), PING_INTERVAL),
            "a connection that has never pinged should ping at once"
        );
    }

    #[rstest]
    #[case::just_sent(0, false)]
    #[case::one_second_short(10, false)]
    #[case::exactly_at_the_interval(11, true)]
    #[case::well_past(60, true)]
    fn a_ping_is_due_once_the_interval_has_elapsed(
        #[case] seconds_since_last: u64,
        #[case] expected: bool,
    ) {
        let sent = Instant::now();
        let now = sent + Duration::from_secs(seconds_since_last);

        assert_eq!(
            expected,
            ping_is_due(Some(sent), now, PING_INTERVAL),
            "{seconds_since_last}s after a ping, with an interval of {PING_INTERVAL:?}"
        );
    }

    #[test]
    fn receive_ping_send_pong() {
        let mut output = Vec::new();
        let mut recv_buffer = Vec::new();

        let ping = Ping::new();
        let payload_received = Message::new(ping.clone())
            .unwrap()
            .get_raw_format()
            .unwrap();

        process_incoming_bytes(&mut output, &mut recv_buffer, &payload_received).unwrap();

        let (response, bytes_read) = MessageReceived::try_parse_message(&output).unwrap();

        assert_eq!(payload_received.len(), bytes_read);

        assert_eq!(0, recv_buffer.len());

        match response {
            Some(PongMessage(pong)) => assert_eq!(ping.nonce, pong.payload.nonce),
            other => panic!("Expected pong message, got {:?}", other),
        }
    }
}
