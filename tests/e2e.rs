//! End-to-end tests that drive the real binary over real sockets.
//!
//! The wire format below is re-implemented rather than imported. That is the
//! point: a conformance test that reuses the node's own encoder cannot catch a
//! bug that is symmetric across encode and decode, and the wire format is the
//! one contract this project actually promises.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const MAGIC: [u8; 4] = [0xf9, 0xbe, 0xb4, 0xd9];
const HEADER: usize = 24;

/// How long to wait for something that should happen.
const PATIENCE: Duration = Duration::from_secs(20);
/// How long to wait before concluding something will *not* happen.
const IMPATIENCE: Duration = Duration::from_secs(3);

fn hash256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(bytes)).into()
}

fn frame(command: &str, payload: &[u8]) -> Vec<u8> {
    let mut name = [0u8; 12];
    name[..command.len()].copy_from_slice(command.as_bytes());

    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&name);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&hash256(payload)[..4]);
    out.extend_from_slice(payload);
    out
}

fn ping(nonce: u64) -> Vec<u8> {
    frame("ping", &nonce.to_le_bytes())
}

fn pong(nonce: u64) -> Vec<u8> {
    frame("pong", &nonce.to_le_bytes())
}

#[derive(Debug, PartialEq, Eq)]
struct Frame {
    command: String,
    nonce: u64,
}

fn parse(buffer: &[u8]) -> Option<(Frame, usize)> {
    if buffer.len() < HEADER {
        return None;
    }

    assert_eq!(
        MAGIC,
        buffer[..4],
        "the node emitted a frame that is not on our network"
    );

    let size = u32::from_le_bytes(buffer[16..20].try_into().unwrap()) as usize;
    if buffer.len() < HEADER + size {
        return None;
    }

    let payload = &buffer[HEADER..HEADER + size];
    assert_eq!(
        &hash256(payload)[..4],
        &buffer[20..24],
        "the node emitted a frame whose checksum does not cover its payload"
    );

    let command = String::from_utf8_lossy(&buffer[4..16])
        .trim_end_matches('\0')
        .to_string();
    assert_eq!(8, payload.len(), "{command} should carry an 8-byte nonce");

    let nonce = u64::from_le_bytes(payload.try_into().unwrap());
    Some((Frame { command, nonce }, HEADER + size))
}

struct Peer {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl Peer {
    fn of(stream: TcpStream) -> Peer {
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        Peer {
            stream,
            buffer: Vec::new(),
        }
    }

    fn dial(address: SocketAddr) -> Peer {
        Peer::of(TcpStream::connect(address).unwrap())
    }

    fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    fn next_frame(&mut self) -> Frame {
        let mut chunk = [0u8; 512];

        loop {
            if let Some((frame, consumed)) = parse(&self.buffer) {
                self.buffer.drain(..consumed);
                return frame;
            }

            let read = self
                .stream
                .read(&mut chunk)
                .expect("the node sent nothing before the read timeout");
            assert_ne!(0, read, "the node closed the connection unexpectedly");
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    /// Everything the node says within `window`. Bounded, so a node that goes
    /// quiet — or that only ever repeats its ping — fails rather than hangs.
    fn frames_within(&mut self, window: Duration) -> Vec<Frame> {
        self.stream.set_read_timeout(Some(window)).unwrap();
        let deadline = Instant::now() + window;
        let mut chunk = [0u8; 512];
        let mut frames = Vec::new();

        loop {
            while let Some((frame, consumed)) = parse(&self.buffer) {
                self.buffer.drain(..consumed);
                frames.push(frame);
            }

            if Instant::now() >= deadline {
                return frames;
            }

            match self.stream.read(&mut chunk) {
                Ok(0) => return frames,
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(e) if timed_out(&e) => return frames,
                Err(e) => panic!("reading from the node failed: {e}"),
            }
        }
    }

    fn pongs_within(&mut self, window: Duration) -> Vec<u64> {
        self.frames_within(window)
            .into_iter()
            .filter(|frame| frame.command == "pong")
            .map(|frame| frame.nonce)
            .collect()
    }

    fn expect_closed(&mut self) {
        self.stream.set_read_timeout(Some(IMPATIENCE)).unwrap();
        let deadline = Instant::now() + IMPATIENCE;
        let mut chunk = [0u8; 512];

        while Instant::now() < deadline {
            match self.stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(_) => continue,
                Err(e) if timed_out(&e) => break,
                Err(_) => return,
            }
        }

        panic!("the node kept the connection open after a message it should have refused");
    }

    fn expect_silence(&mut self) {
        self.stream.set_read_timeout(Some(IMPATIENCE)).unwrap();
        let mut chunk = [0u8; 512];

        match self.stream.read(&mut chunk) {
            Err(e) if timed_out(&e) => (),
            Ok(read) => panic!("expected silence, got {read} more bytes"),
            Err(e) => panic!("expected silence, got {e}"),
        }
    }
}

fn timed_out(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Sandbox {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "avicoin-e2e-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));

        fs::create_dir_all(&path).unwrap();
        Sandbox(path)
    }

    fn with_config(contents: &str) -> Sandbox {
        let sandbox = Sandbox::new();
        fs::write(sandbox.0.join("config.toml"), contents).unwrap();
        sandbox
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Node {
    child: Child,
    lines: Receiver<String>,
    _sandbox: Sandbox,
}

impl Node {
    fn start(args: &[&str]) -> Node {
        Node::start_in(Sandbox::new(), args)
    }

    fn start_in(sandbox: Sandbox, args: &[&str]) -> Node {
        let mut child = Command::new(env!("CARGO_BIN_EXE_avicoin"))
            .args(args)
            .current_dir(&sandbox.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("could not launch the node binary");

        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();

        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    return;
                }
            }
        });

        Node {
            child,
            lines,
            _sandbox: sandbox,
        }
    }

    fn listening_on(&self) -> SocketAddr {
        self.line_containing("Listening on")
            .rsplit_once(' ')
            .expect("the listening line should end in an address")
            .1
            .parse()
            .expect("the node announced an address that does not parse")
    }

    fn line_containing(&self, needle: &str) -> String {
        // A deadline, not a per-line timeout: a node that keeps saying something
        // else would otherwise keep resetting the clock forever.
        let deadline = Instant::now() + PATIENCE;
        let mut seen = Vec::new();

        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if line.contains(needle) => return line,
                Ok(line) => seen.push(line),
                Err(_) => break,
            }
        }

        panic!("nothing containing {needle:?} within {PATIENCE:?}; the node said: {seen:#?}")
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_and_fail(sandbox: &Sandbox, args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_avicoin"))
        .args(args)
        .current_dir(&sandbox.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("could not launch the node binary");

    let deadline = Instant::now() + IMPATIENCE;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        thread::sleep(Duration::from_millis(20));
    }

    let _ = child.kill();
    let _ = child.wait();
    panic!("the node was expected to fail at startup, but it is still running");
}

fn a_free_port() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

/// `TcpListener::accept` blocks forever, which turns "the node never dialled"
/// from a failure into a hung suite.
fn accept_within(listener: &TcpListener, patience: Duration) -> Option<TcpStream> {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + patience;

    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return Some(stream);
            }
            Err(e) if timed_out(&e) => thread::sleep(Duration::from_millis(10)),
            Err(e) => panic!("accept failed: {e}"),
        }
    }

    None
}

fn expect_dialled(listener: &TcpListener) -> Peer {
    let stream = accept_within(listener, PATIENCE).expect("the node never dialled us");
    Peer::of(stream)
}

// --- connections ------------------------------------------------------------

#[test]
fn a_node_pings_whoever_dials_it() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let mut peer = Peer::dial(node.listening_on());

    assert_eq!("ping", peer.next_frame().command);
}

#[test]
fn a_node_answers_a_ping_with_a_pong_carrying_the_same_nonce() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let mut peer = Peer::dial(node.listening_on());

    peer.send(&ping(0x0123_4567_89ab_cdef));

    assert_eq!(vec![0x0123_4567_89ab_cdef], peer.pongs_within(IMPATIENCE));
}

#[test]
fn a_node_dials_every_address_it_was_given() {
    let (first, first_address) = a_free_port();
    let (second, second_address) = a_free_port();

    let _node = Node::start(&[
        "--host-address",
        "127.0.0.1:0",
        "--addresses-to-connect",
        &first_address.to_string(),
        "--addresses-to-connect",
        &second_address.to_string(),
    ]);

    for listener in [first, second] {
        assert_eq!("ping", expect_dialled(&listener).next_frame().command);
    }
}

#[test]
fn a_pong_is_accepted_and_does_not_provoke_another_pong() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let mut peer = Peer::dial(node.listening_on());

    let opening = peer.next_frame();
    assert_eq!("ping", opening.command);
    peer.send(&pong(opening.nonce));

    node.line_containing("Pong received");
    peer.expect_silence();
}

#[test]
fn two_real_nodes_complete_a_ping_pong_round_trip() {
    let listener = Node::start(&["--host-address", "127.0.0.1:0"]);
    let listener_address = listener.listening_on();

    let dialler = Node::start(&[
        "--host-address",
        "127.0.0.1:0",
        "--addresses-to-connect",
        &listener_address.to_string(),
    ]);

    // A pong only arrives if the other node parsed our ping, framed a reply,
    // and we parsed that — the whole path, in both directions.
    listener.line_containing("Pong received");
    dialler.line_containing("Pong received");
}

// --- a bad peer does not take the node down ---------------------------------

#[test]
fn a_peer_speaking_another_networks_magic_bytes_is_dropped() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let address = node.listening_on();

    let mut villain = Peer::dial(address);
    let mut foreign = ping(1);
    foreign[0] ^= 0xff;
    villain.send(&foreign);
    villain.expect_closed();

    assert_eq!("ping", Peer::dial(address).next_frame().command);
}

#[test]
fn a_peer_claiming_a_four_gigabyte_payload_is_dropped() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let address = node.listening_on();

    let mut villain = Peer::dial(address);
    let mut header = ping(1)[..HEADER].to_vec();
    header[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    villain.send(&header);
    villain.expect_closed();

    assert_eq!("ping", Peer::dial(address).next_frame().command);
}

#[test]
fn a_peer_whose_payload_does_not_match_its_checksum_is_dropped() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let address = node.listening_on();

    let mut villain = Peer::dial(address);
    let mut corrupted = ping(1);
    *corrupted.last_mut().unwrap() ^= 0xff;
    villain.send(&corrupted);
    villain.expect_closed();

    assert_eq!("ping", Peer::dial(address).next_frame().command);
}

#[test]
fn a_peer_sending_an_unknown_command_is_dropped() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let address = node.listening_on();

    let mut villain = Peer::dial(address);
    villain.send(&frame("notacommand", &7u64.to_le_bytes()));
    villain.expect_closed();

    assert_eq!("ping", Peer::dial(address).next_frame().command);
}

#[test]
fn a_peer_that_vanishes_mid_message_does_not_take_the_node_down() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let address = node.listening_on();

    let mut deserter = Peer::dial(address);
    deserter.send(&ping(1)[..HEADER - 4]);
    drop(deserter);

    assert_eq!("ping", Peer::dial(address).next_frame().command);
}

#[test]
fn a_peer_that_dribbles_a_message_one_byte_at_a_time_is_still_understood() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let mut peer = Peer::dial(node.listening_on());

    for byte in ping(0xdead_beef) {
        peer.send(&[byte]);
        thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(
        vec![0xdead_beef],
        peer.pongs_within(IMPATIENCE),
        "a message split across reads must still be answered"
    );
}

#[test]
fn two_messages_arriving_in_one_read_are_both_answered() {
    let node = Node::start(&["--host-address", "127.0.0.1:0"]);
    let mut peer = Peer::dial(node.listening_on());

    let mut both = ping(11);
    both.extend_from_slice(&ping(22));
    peer.send(&both);

    assert_eq!(
        vec![11, 22],
        peer.pongs_within(IMPATIENCE),
        "both messages in one read must be answered, in order"
    );
}

// --- configuration ----------------------------------------------------------

#[test]
fn with_no_config_and_no_arguments_a_node_uses_the_documented_default() {
    let sandbox = Sandbox::new();
    let node = Node::start_in(sandbox, &[]);

    // The default port is fixed, so it may already be taken on this machine.
    // Either outcome names the address, which is what is under test.
    let line = node.line_containing("127.0.0.1:34352");
    assert!(
        line.contains("Listening on") || line.contains("could not listen"),
        "got: {line}"
    );
}

#[test]
fn a_config_file_supplies_the_listening_address() {
    let (probe, probe_address) = a_free_port();
    let sandbox = Sandbox::with_config(&format!(
        "[server]\nhost_address = \"127.0.0.1:0\"\naddresses_to_connect = [\"{probe_address}\"]\n"
    ));

    let _node = Node::start_in(sandbox, &[]);

    assert_eq!("ping", expect_dialled(&probe).next_frame().command);
}

#[test]
fn a_command_line_address_overrides_the_config_file() {
    let (ignored, ignored_address) = a_free_port();
    let (chosen, chosen_address) = a_free_port();
    let sandbox = Sandbox::with_config(&format!(
        "[server]\nhost_address = \"127.0.0.1:0\"\naddresses_to_connect = [\"{ignored_address}\"]\n"
    ));

    let _node = Node::start_in(
        sandbox,
        &[
            "--host-address",
            "127.0.0.1:0",
            "--addresses-to-connect",
            &chosen_address.to_string(),
        ],
    );

    assert_eq!("ping", expect_dialled(&chosen).next_frame().command);

    assert!(
        accept_within(&ignored, IMPATIENCE).is_none(),
        "the config file's peer was overridden and must not be dialled"
    );
}

// --- a bad start is a failed process, not a limping node --------------------

#[test]
fn an_address_that_cannot_be_bound_fails_the_process() {
    let (occupied, occupied_address) = a_free_port();
    let sandbox = Sandbox::new();

    let output = start_and_fail(&sandbox, &["--host-address", &occupied_address.to_string()]);

    assert!(!output.status.success(), "a taken port must fail the node");
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(
        complaint.contains(&occupied_address.to_string()),
        "the failure should name the address it could not bind, got: {complaint}"
    );
    drop(occupied);
}

#[test]
fn a_malformed_address_in_the_config_file_fails_the_process() {
    let sandbox = Sandbox::with_config("[server]\nhost_address = \"not-an-address\"\n");

    let output = start_and_fail(&sandbox, &[]);

    assert!(!output.status.success(), "a bad address must fail the node");
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(
        complaint.contains("host_address") && complaint.contains("not-an-address"),
        "the failure should name the field and the value, got: {complaint}"
    );
}

#[test]
fn an_unknown_key_in_the_config_file_fails_the_process() {
    let sandbox = Sandbox::with_config("[server]\nhost_adress = \"127.0.0.1:1\"\n");

    let output = start_and_fail(&sandbox, &[]);

    assert!(
        !output.status.success(),
        "a typo in config.toml must fail the node rather than silently defaulting"
    );
}

#[test]
fn an_unparseable_config_file_fails_the_process() {
    let sandbox = Sandbox::with_config("this is not toml at all\n");

    let output = start_and_fail(&sandbox, &[]);

    assert!(!output.status.success(), "broken toml must fail the node");
}

#[test]
fn an_unreachable_peer_is_logged_and_the_node_keeps_listening() {
    let (closed, closed_address) = a_free_port();
    drop(closed);

    let node = Node::start(&[
        "--host-address",
        "127.0.0.1:0",
        "--addresses-to-connect",
        &closed_address.to_string(),
    ]);
    let address = node.listening_on();

    node.line_containing("Could not connect");
    assert_eq!("ping", Peer::dial(address).next_frame().command);
}
