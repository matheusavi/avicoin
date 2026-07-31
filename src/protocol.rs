use crate::messages::message::MessageReceived::{PingMessage, PongMessage};
use crate::messages::message::{Message, MessageReceived};
use crate::messages::ping::Ping;
use crate::messages::pong::Pong;
use crate::node::SharedNode;
use anyhow::{anyhow, Result};
use std::io::{ErrorKind, Read, Write};
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

struct ShutdownOnDrop(TcpStream);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        // The reader parks in read() with no timeout; only a shutdown wakes it.
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

fn handle_connection(stream: TcpStream, peer_addr: SocketAddr, node: SharedNode) -> Result<()> {
    let write_half = ShutdownOnDrop(stream.try_clone()?);
    let (outbound, queued) = mpsc::channel();

    let host_address = node.lock().expect("node lock poisoned").config.host_address;
    println!("{host_address} is handling a connection from {peer_addr}");

    let writer = thread::spawn(move || write_loop(&write_half.0, queued, PING_INTERVAL));

    let read_result = read_loop(stream, peer_addr, outbound);

    match writer.join() {
        Ok(write_result) => read_result.and(write_result),
        Err(_) => Err(anyhow!("writer thread panicked")),
    }
}

fn write_loop<W: Write>(
    mut writer: W,
    queued: Receiver<Vec<u8>>,
    ping_interval: Duration,
) -> Result<()> {
    let mut next_ping = Instant::now();

    loop {
        if Instant::now() >= next_ping {
            writer.write_all(&Message::new(Ping::new())?.get_raw_format()?)?;
            next_ping = Instant::now() + ping_interval;
        }

        match queued.recv_timeout(next_ping.saturating_duration_since(Instant::now())) {
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
        match reader.read(&mut buffer) {
            Ok(0) => {
                println!("Connection with {peer_addr} closed");
                return Ok(());
            }
            Ok(n) => process_incoming_bytes(&outbound, &mut recv_buffer, &buffer[..n])?,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
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

    fn framed<P: crate::messages::message::Payload>(payload: P) -> Vec<u8> {
        Message::new(payload).unwrap().get_raw_format().unwrap()
    }

    fn framed_ping() -> (Vec<u8>, u64) {
        let ping = Ping::new();
        let nonce = ping.nonce;
        (framed(ping), nonce)
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
        let (outbound, queued) = mpsc::channel();
        drop(outbound);

        let mut output = Vec::new();
        write_loop(&mut output, queued, NEVER).unwrap();

        assert!(
            matches!(parse_all(&output).as_slice(), [PingMessage(_)]),
            "a new connection should ping at once, not after an hour"
        );
    }

    #[test]
    fn a_message_enqueued_from_another_thread_is_written_to_the_peer() {
        let (outbound, queued) = mpsc::channel();
        let ping = Ping::new();
        let nonce = ping.nonce;
        let pong = framed(Pong::new(ping).unwrap());

        let sender = thread::spawn(move || {
            // The writer must already be parked in recv_timeout, or this proves
            // only that a drained queue is written, not that a live writer wakes.
            thread::sleep(Duration::from_millis(50));
            outbound.send(pong).unwrap();
        });

        let mut output = Vec::new();
        write_loop(&mut output, queued, NEVER).unwrap();
        sender.join().unwrap();

        match parse_all(&output).as_slice() {
            [PingMessage(_), PongMessage(pong)] => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected the opening ping then the enqueued pong, got {other:?}"),
        }
    }

    #[test]
    fn pings_recur_at_the_configured_interval() {
        let interval = Duration::from_millis(20);
        let run_for = Duration::from_millis(300);

        let (outbound, queued) = mpsc::channel::<Vec<u8>>();
        let holder = thread::spawn(move || {
            thread::sleep(run_for);
            drop(outbound);
        });

        let mut output = Vec::new();
        write_loop(&mut output, queued, interval).unwrap();
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
        let (outbound, queued) = mpsc::channel();
        let mut recv_buffer = Vec::new();
        let (ping, nonce) = framed_ping();

        process_incoming_bytes(&outbound, &mut recv_buffer, &ping).unwrap();

        assert!(recv_buffer.is_empty(), "the ping should be fully consumed");

        let reply = queued.try_recv().expect("a ping must be answered");
        match parse_all(&reply).as_slice() {
            [PongMessage(pong)] => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected a pong, got {other:?}"),
        }
        assert!(queued.try_recv().is_err(), "one ping, one pong");
    }

    #[test]
    fn an_oversized_header_fails_the_connection_rather_than_being_awaited() {
        let (outbound, queued) = mpsc::channel();
        let mut recv_buffer = Vec::new();
        let header = crate::messages::message::header_claiming(u32::MAX);

        let error = process_incoming_bytes(&outbound, &mut recv_buffer, &header)
            .expect_err("a header claiming 4 GB must fail the connection, not be waited on");

        assert!(format!("{error:#}").contains("too large"), "got: {error:#}");
        assert!(
            queued.try_recv().is_err(),
            "nothing should be queued in reply to a header that was refused"
        );
    }

    #[test]
    fn an_interrupted_read_does_not_end_the_connection() {
        struct InterruptsOnce {
            ping: Vec<u8>,
            interrupted: bool,
        }

        impl Read for InterruptsOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::Error::from(ErrorKind::Interrupted));
                }
                let taken = self.ping.len();
                buffer[..taken].copy_from_slice(&self.ping);
                self.ping.clear();
                Ok(taken)
            }
        }

        let (outbound, queued) = mpsc::channel();
        let (ping, nonce) = framed_ping();
        let reader = InterruptsOnce {
            ping,
            interrupted: false,
        };

        read_loop(reader, "127.0.0.1:1".parse().unwrap(), outbound)
            .expect("a signal-interrupted read must be retried, not fail the connection");

        let reply = queued.try_recv().expect("the ping after the interrupt");
        match parse_all(&reply).as_slice() {
            [PongMessage(pong)] => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected a pong, got {other:?}"),
        }
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
        let (accepted, peer_addr) = listener.accept().unwrap();

        (peer, accepted, peer_addr)
    }

    fn a_node() -> SharedNode {
        Node::shared(Config {
            host_address: "127.0.0.1:34352".parse().unwrap(),
            addresses_to_connect: Vec::new(),
        })
    }

    #[test]
    fn a_connection_pings_its_peer_and_answers_the_peers_ping() {
        let (mut peer, accepted, peer_addr) = a_connected_pair();

        thread::spawn(move || handle_connection(accepted, peer_addr, a_node()));

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
        let (peer, accepted, peer_addr) = a_connected_pair();
        let (done, finished) = mpsc::channel();

        thread::spawn(move || {
            let _ = handle_connection(accepted, peer_addr, a_node());
            done.send(()).unwrap();
        });

        drop(peer);

        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("a connection whose peer is gone must not leave a thread parked");
    }

    #[test]
    fn losing_the_write_half_wakes_a_reader_that_has_no_timeout() {
        let (peer, accepted, _) = a_connected_pair();
        let write_half = ShutdownOnDrop(accepted.try_clone().unwrap());
        let mut read_half = accepted;
        let (done, finished) = mpsc::channel();

        thread::spawn(move || {
            let mut buffer = [0u8; 16];
            let _ = read_half.read(&mut buffer);
            done.send(()).unwrap();
        });

        thread::sleep(Duration::from_millis(50));
        assert!(
            finished.try_recv().is_err(),
            "the peer is silent but alive, so the reader must still be parked"
        );

        drop(write_half);

        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("dropping the write half must wake a reader parked in read()");
        drop(peer);
    }
}
