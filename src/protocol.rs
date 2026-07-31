use crate::messages::message::MessageReceived::{PingMessage, PongMessage};
use crate::messages::message::{Message, MessageReceived};
use crate::messages::ping::Ping;
use crate::messages::pong::Pong;
use crate::node::SharedNode;
use anyhow::Result;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const PING_INTERVAL: Duration = Duration::from_secs(11);

pub fn connect(addr: SocketAddr, node: SharedNode) -> Result<()> {
    let stream = TcpStream::connect(addr)?;
    spawn_connection(stream, node);

    Ok(())
}

pub fn listen(listener: TcpListener, node: SharedNode) -> Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => spawn_connection(stream, Arc::clone(&node)),
            Err(e) => println!("Could not accept a connection: {e}"),
        }
    }

    Ok(())
}

fn spawn_connection(stream: TcpStream, node: SharedNode) {
    thread::spawn(move || {
        let peer = match stream.peer_addr() {
            Ok(peer) => peer,
            Err(e) => {
                println!("Dropping a connection with no resolvable peer address: {e}");
                return;
            }
        };

        if let Err(e) = handle_connection(stream, peer, node) {
            println!("Connection with {peer} ended: {e:#}");
        }
    });
}

fn handle_connection(stream: TcpStream, peer_addr: SocketAddr, node: SharedNode) -> Result<()> {
    let write_half = stream.try_clone()?;
    let (outbound, inbound) = mpsc::channel();

    let host_address = node.lock().expect("node lock poisoned").config.host_address;
    println!("{host_address} is handling a connection from {peer_addr}");

    let writer = thread::spawn(move || {
        if let Err(e) = write_loop(&write_half, inbound, PING_INTERVAL) {
            println!("Writer for {peer_addr} stopped: {e:#}");
        }
        // Unblocks the reader, which is parked in read() with no timeout.
        let _ = write_half.shutdown(Shutdown::Both);
    });

    let read_result = read_loop(stream, peer_addr, outbound);
    let _ = writer.join();

    read_result
}

fn write_loop<W: Write>(
    mut writer: W,
    inbound: Receiver<Vec<u8>>,
    ping_interval: Duration,
) -> Result<()> {
    let mut next_ping = Instant::now();

    loop {
        let now = Instant::now();
        if now >= next_ping {
            writer.write_all(&Message::new(Ping::new())?.get_raw_format()?)?;
            next_ping = now + ping_interval;
        }

        match inbound.recv_timeout(next_ping.saturating_duration_since(Instant::now())) {
            Ok(bytes) => writer.write_all(&bytes)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn read_loop<R: Read>(
    mut reader: R,
    peer_addr: SocketAddr,
    outbound: Sender<Vec<u8>>,
) -> Result<()> {
    let mut buffer = [0u8; 4096];
    let mut recv_buffer: Vec<u8> = Vec::new();

    loop {
        match reader.read(&mut buffer)? {
            0 => {
                println!("Connection with {peer_addr} closed");
                return Ok(());
            }
            n => process_incoming_bytes(&outbound, &mut recv_buffer, &buffer[..n])?,
        }
    }
}

fn process_incoming_bytes(
    outbound: &Sender<Vec<u8>>,
    recv_buffer: &mut Vec<u8>,
    buffer: &[u8],
) -> Result<()> {
    recv_buffer.extend(buffer);
    while let (Some(message), bytes_consumed) = MessageReceived::try_parse_message(recv_buffer)? {
        recv_buffer.drain(0..bytes_consumed);

        handle_messages(outbound, message)?
    }
    Ok(())
}

fn handle_messages(outbound: &Sender<Vec<u8>>, message: MessageReceived) -> Result<()> {
    match message {
        PingMessage(ping) => {
            println!("Ping received {:?}", ping);
            let pong = Pong::new(ping.payload)?;
            outbound.send(Message::new(pong)?.get_raw_format()?)?;
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
    use crate::config::Config;
    use crate::node::Node;

    const NEVER: Duration = Duration::from_secs(3600);

    fn framed_ping() -> (Vec<u8>, u64) {
        let ping = Ping::new();
        let nonce = ping.nonce;
        (Message::new(ping).unwrap().get_raw_format().unwrap(), nonce)
    }

    fn parse_all(bytes: &[u8]) -> Vec<MessageReceived> {
        let mut rest = bytes;
        let mut messages = Vec::new();

        while let (Some(message), consumed) = MessageReceived::try_parse_message(rest).unwrap() {
            messages.push(message);
            rest = &rest[consumed..];
        }

        assert!(rest.is_empty(), "{} trailing bytes", rest.len());
        messages
    }

    #[test]
    fn the_first_ping_is_written_without_waiting_for_the_interval() {
        let (outbound, inbound) = mpsc::channel();
        drop(outbound);

        let mut output = Vec::new();
        write_loop(&mut output, inbound, NEVER).unwrap();

        assert!(
            matches!(parse_all(&output).as_slice(), [PingMessage(_)]),
            "a new connection should ping at once, not after an hour"
        );
    }

    #[test]
    fn a_message_enqueued_from_another_thread_is_written_to_the_peer() {
        let (outbound, inbound) = mpsc::channel();
        let ping = Ping::new();
        let nonce = ping.nonce;
        let pong = Message::new(Pong::new(ping).unwrap())
            .unwrap()
            .get_raw_format()
            .unwrap();

        thread::spawn(move || outbound.send(pong).unwrap())
            .join()
            .unwrap();

        let mut output = Vec::new();
        write_loop(&mut output, inbound, NEVER).unwrap();

        match parse_all(&output).as_slice() {
            [PingMessage(_), PongMessage(pong)] => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected the opening ping then the enqueued pong, got {other:?}"),
        }
    }

    #[test]
    fn pings_recur_at_the_configured_interval() {
        let interval = Duration::from_millis(20);
        let run_for = Duration::from_millis(300);

        let (outbound, inbound) = mpsc::channel::<Vec<u8>>();
        let holder = thread::spawn(move || {
            thread::sleep(run_for);
            drop(outbound);
        });

        let mut output = Vec::new();
        write_loop(&mut output, inbound, interval).unwrap();
        holder.join().unwrap();

        let pings = parse_all(&output).len();
        let expected = run_for.as_millis() / interval.as_millis();

        assert!(
            pings >= 5,
            "{pings} pings in {run_for:?} at a {interval:?} interval; \
             a timer that only fires when something else wakes it would send ~1, not ~{expected}"
        );
        assert!(
            pings <= 40,
            "{pings} pings is a busy loop, not a {interval:?} interval"
        );
    }

    #[test]
    fn an_inbound_ping_is_answered_with_a_pong_on_the_outbound_channel() {
        let (outbound, inbound) = mpsc::channel();
        let mut recv_buffer = Vec::new();
        let (ping, nonce) = framed_ping();

        process_incoming_bytes(&outbound, &mut recv_buffer, &ping).unwrap();

        assert!(recv_buffer.is_empty(), "the ping should be fully consumed");

        let queued = inbound.try_recv().expect("a ping must be answered");
        match parse_all(&queued).as_slice() {
            [PongMessage(pong)] => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected a pong, got {other:?}"),
        }
        assert!(inbound.try_recv().is_err(), "one ping, one pong");
    }

    #[test]
    fn an_oversized_header_fails_the_connection_rather_than_being_awaited() {
        let (outbound, inbound) = mpsc::channel();
        let mut recv_buffer = Vec::new();
        let header = crate::messages::message::header_claiming(u32::MAX);

        let error = process_incoming_bytes(&outbound, &mut recv_buffer, &header)
            .expect_err("a header claiming 4 GB must fail the connection, not be waited on");

        assert!(format!("{error:#}").contains("too large"), "got: {error:#}");
        assert!(
            inbound.try_recv().is_err(),
            "nothing should be queued in reply to a header that was refused"
        );
    }

    fn next_message(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> MessageReceived {
        let mut chunk = [0u8; 512];

        loop {
            if let (Some(message), consumed) = MessageReceived::try_parse_message(buffer)
                .expect("peer sent an unparseable message")
            {
                buffer.drain(0..consumed);
                return message;
            }

            let read = stream.read(&mut chunk).expect("peer went quiet");
            assert_ne!(0, read, "peer closed before sending the expected message");
            buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn a_connected_pair() -> (TcpStream, TcpStream, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        let peer = TcpStream::connect(address).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let (accepted, _) = listener.accept().unwrap();

        (peer, accepted, address)
    }

    fn a_node(address: SocketAddr) -> SharedNode {
        Node::shared(Config {
            host_address: address,
            addresses_to_connect: Vec::new(),
        })
    }

    #[test]
    fn a_connection_pings_its_peer_and_answers_the_peers_ping() {
        let (mut peer, accepted, address) = a_connected_pair();

        thread::spawn(move || handle_connection(accepted, address, a_node(address)));

        let mut buffer = Vec::new();
        assert!(
            matches!(next_message(&mut peer, &mut buffer), PingMessage(_)),
            "a connection should open by pinging its peer"
        );

        let (ping, nonce) = framed_ping();
        peer.write_all(&ping).unwrap();

        match next_message(&mut peer, &mut buffer) {
            PongMessage(pong) => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected a pong for our ping, got {other:?}"),
        }
    }

    #[test]
    fn both_threads_end_when_the_peer_disconnects() {
        let (peer, accepted, address) = a_connected_pair();
        let (done, finished) = mpsc::channel();

        thread::spawn(move || {
            let _ = handle_connection(accepted, address, a_node(address));
            done.send(()).unwrap();
        });

        drop(peer);

        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("a connection whose peer is gone must not leave a thread parked");
    }
}
