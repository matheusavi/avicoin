use crate::node::{record, SharedNode};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How many requests are served at once. Fixed, because a thread per
/// connection is a thread per stranger.
const WORKERS: usize = 4;

/// How many connections may wait for a worker. Past this a connection is
/// closed rather than queued: a queue that grows is a queue a stranger fills.
const WAITING: usize = 16;

/// How long a connection may take to say what it wants, and to read what it is
/// told. A socket that goes quiet costs a worker until this expires and not a
/// moment longer — nothing here waits on a stranger indefinitely.
const PATIENCE: Duration = Duration::from_secs(10);

/// The largest request head — the request line and every header. A client that
/// never sends a newline would otherwise be asking us to buffer forever.
const MAX_HEAD: usize = 8 * 1024;

/// The largest body. `POST /tx` carries a transaction, and `MAX_TRANSACTION_SIZE`
/// is 100,000 bytes of it; hex doubles that.
const MAX_BODY: usize = 256 * 1024;

/// The largest response. A collection endpoint is capped well below this; the
/// ceiling is what stops a bug becoming a memory attack.
const MAX_RESPONSE: usize = 1 << 20;

/// One HTTP request, as much of it as anything here cares about.
pub struct Asked {
    pub method: String,
    pub url: String,
    pub body: Vec<u8>,
}

/// Binding is the caller's, not this thread's: a port already taken must fail
/// the process, the same reason `main` binds the P2P listener itself.
pub fn bind(address: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(address).with_context(|| format!("could not serve the API on {address}"))
}

/// Accepts on this thread and answers on a fixed pool.
///
/// **HTTP is hand-rolled here**, which the dependency posture argues for: the
/// crate it replaces bounded nothing — no read timeout, a request line read
/// into a `Vec` with no cap, a thread per connection, and an accept error that
/// dropped the listener in silence. Every one of those is a rule this project
/// enforces on the P2P side, and an API meant to face the public cannot have
/// weaker ones than the protocol behind it.
///
/// Each worker takes the node lock, copies out what it needs, and **releases
/// it before writing a byte** — the rule `node::record` follows for stdout and
/// `broadcast` follows with `try_send`. A client that stops reading must cost
/// the peers nothing.
pub fn serve(listener: TcpListener, node: SharedNode) -> Result<()> {
    let (queue, waiting) = sync_channel::<TcpStream>(WAITING);
    let waiting = Arc::new(Mutex::new(waiting));

    for _ in 0..WORKERS {
        let waiting = Arc::clone(&waiting);
        let node = Arc::clone(&node);
        thread::spawn(move || work(&waiting, &node));
    }

    accept(listener, &queue, &node);

    Ok(())
}

fn accept(listener: TcpListener, queue: &SyncSender<TcpStream>, node: &SharedNode) {
    loop {
        match listener.accept() {
            // A full queue is answered rather than left hanging, and the
            // connection goes. Closing it is what makes the bound a bound.
            Ok((stream, _)) => {
                if let Err(returned) = queue.try_send(stream) {
                    let mut refused = match returned {
                        std::sync::mpsc::TrySendError::Full(stream) => stream,
                        std::sync::mpsc::TrySendError::Disconnected(stream) => stream,
                    };
                    let _ = write(&mut refused, 503, &json!({"error": "the API is busy"}));
                }
            }
            // Not fatal. Running out of descriptors is a condition that
            // passes, and a listener that gave up on one would be an API that
            // never came back and never said why.
            Err(why) => {
                record(node, format!("API: could not accept a connection: {why}"));
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn work(waiting: &Mutex<Receiver<TcpStream>>, node: &SharedNode) {
    loop {
        let taken = waiting.lock().expect("api queue poisoned").recv();
        let Ok(mut stream) = taken else { return };

        let _ = stream.set_read_timeout(Some(PATIENCE));
        let _ = stream.set_write_timeout(Some(PATIENCE));
        answer(&mut stream, node);
    }
}

fn answer(stream: &mut TcpStream, node: &SharedNode) {
    let asked = match read_request(stream) {
        Ok(asked) => asked,
        Err(why) => {
            let _ = write(stream, 400, &json!({ "error": why }));
            return;
        }
    };

    // One handler's panic must not take a worker with it, the same way one
    // peer's panic does not take the listener down. The lock is cleared
    // afterwards because a panic taken *under* it would otherwise poison it
    // for every peer thread and the miner — the API failing is not a reason
    // for the node to stop being a node.
    let routed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| route(&asked, node)));

    let (status, body) = match routed {
        Ok(answer) => answer,
        Err(_) => {
            node.clear_poison();
            record(node, "API: a request panicked".to_string());
            (500, json!({"error": "the node could not answer that"}))
        }
    };

    let _ = write(stream, status, &body);
}

fn write(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let rendered = body.to_string();
    let (status, rendered) = if rendered.len() > MAX_RESPONSE {
        (
            500,
            json!({"error": "the response is too large"}).to_string(),
        )
    } else {
        (status, rendered)
    };

    let head = format!(
        "HTTP/1.1 {status} {}\r\n         Content-Type: application/json\r\n         Content-Length: {}\r\n         Connection: close\r\n\r\n",
        reason(status),
        rendered.len()
    );

    stream.write_all(head.as_bytes())?;
    stream.write_all(rendered.as_bytes())?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

/// HTTP/1.1, as much of it as this needs, with every read bounded.
fn read_request(stream: &mut TcpStream) -> std::result::Result<Asked, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|why| why.to_string())?);
    let mut head = Vec::new();

    loop {
        let mut line = Vec::new();
        // `take` is the cap: a client that never sends a newline is asking us
        // to buffer forever, and `read_until` would.
        let read = (&mut reader)
            .take((MAX_HEAD - head.len()) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|why| format!("could not read the request: {why}"))?;

        if read == 0 {
            return Err("the request ended before its headers did".to_string());
        }
        head.extend_from_slice(&line);
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if head.len() >= MAX_HEAD {
            return Err(format!("a request head is at most {MAX_HEAD} bytes"));
        }
    }

    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    let mut request = lines
        .next()
        .ok_or("the request is empty")?
        .split_whitespace();
    let method = request.next().ok_or("the request has no method")?;
    let url = request.next().ok_or("the request has no path")?;
    // The version, checked but not kept: without it "this is not HTTP" parses
    // as a `this` request for `is`, and gets answered as one.
    match request.next() {
        Some(version) if version.starts_with("HTTP/") => {}
        _ => return Err("the request line is not HTTP".to_string()),
    }

    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>())
        .transpose()
        .map_err(|_| "Content-Length is not a number".to_string())?
        .unwrap_or(0);

    if length > MAX_BODY {
        return Err(format!("a body is at most {MAX_BODY} bytes"));
    }

    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|why| format!("could not read the body: {why}"))?;

    Ok(Asked {
        method: method.to_string(),
        url: url.to_string(),
        body,
    })
}

/// **Takes the node lock and gives it back before returning.** Nothing here
/// may borrow from the guard, because everything after this writes to a
/// socket a stranger controls the read end of.
fn route(asked: &Asked, node: &SharedNode) -> (u16, Value) {
    match (asked.method.as_str(), path(&asked.url)) {
        ("GET", "/status") => (200, status(node)),
        ("GET", _) => (404, json!({"error": "no such endpoint"})),
        (method, _) => (
            405,
            json!({ "error": format!("{method} is not something this endpoint answers") }),
        ),
    }
}

/// The path alone. A query string is not part of what an endpoint is.
fn path(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

fn status(node: &SharedNode) -> Value {
    let held = node.lock().expect("node lock poisoned");

    json!({
        "network": held.config.network.name,
        "height": held.chain.height(),
        "tip": held.chain.tip().to_string(),
        "peers": held.peers.len(),
        "mempool": held.mempool.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::node::Node;
    use crate::params::TESTNET;
    use crate::wallet::Wallet;

    fn get(url: &str) -> Asked {
        Asked {
            method: "GET".to_string(),
            url: url.to_string(),
            body: Vec::new(),
        }
    }

    fn a_node() -> SharedNode {
        let genesis = TESTNET.genesis().unwrap();
        Node::shared(
            Config {
                api_address: None,
                data_dir: std::path::PathBuf::new(),
                mine: false,
                network: &TESTNET,
                host_address: "127.0.0.1:0".parse().unwrap(),
                addresses_to_connect: Vec::new(),
            },
            &genesis,
            Wallet::new(),
        )
        .unwrap()
    }

    #[test]
    fn status_reports_the_tip_the_node_is_on() {
        let node = a_node();
        let (tip, height) = {
            let held = node.lock().unwrap();
            (held.chain.tip().to_string(), held.chain.height())
        };

        let (status, body) = route(&get("/status"), &node);

        assert_eq!(status, 200);
        assert_eq!(body["network"], "test");
        assert_eq!(body["tip"], tip);
        assert_eq!(body["height"], height);
        assert_eq!(body["peers"], 0);
        assert_eq!(body["mempool"], 0);
    }

    /// Everything after `route` writes to a socket whose read end a stranger
    /// controls. A handler that returned while still holding the node lock —
    /// by keeping the guard in what it returns, say — would stall every peer
    /// behind a client that stopped reading.
    ///
    /// This is what the seam can prove, and it is worth being plain about
    /// what it cannot: `route` returns `(u16, Value)`, a type that *cannot*
    /// borrow the guard, so the compiler enforces most of it. What the test
    /// adds is the case the type does not cover — a handler that put the guard
    /// somewhere it outlives the call. That the write happens after the
    /// routing is structural: `answer` calls `route`, takes its value, and
    /// only then touches the socket.
    #[test]
    fn routing_gives_the_node_lock_back_before_it_returns() {
        let node = a_node();

        let _ = route(&get("/status"), &node);

        assert!(
            node.try_lock().is_ok(),
            "a handler must not hold the lock past its own return"
        );
    }

    /// A panic taken while the node lock is held would poison it, and every
    /// peer thread and the miner would then die on their next `lock`. The API
    /// failing is not a reason for the node to stop being a node.
    #[test]
    fn a_panic_under_the_node_lock_does_not_poison_it_for_everybody_else() {
        let node = a_node();

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = node.lock().expect("node lock poisoned");
            panic!("a handler that got something wrong");
        }));

        assert!(panicked.is_err());
        assert!(node.is_poisoned(), "this is the hazard, not a hypothetical");
        node.clear_poison();
        assert!(node.lock().is_ok(), "and the node goes on being a node");
    }

    #[test]
    fn an_unknown_path_is_a_404_with_a_reason() {
        let node = a_node();

        let (status, body) = route(&get("/nothing-here"), &node);

        assert_eq!(status, 404);
        assert!(body["error"].is_string());
    }

    /// A query string is not part of what an endpoint is, and `/status?x=1`
    /// asks the same question `/status` does.
    #[test]
    fn a_query_string_is_not_part_of_the_path() {
        assert_eq!(path("/status?since=4"), "/status");
        assert_eq!(path("/status"), "/status");
        assert_eq!(path("/block/height/7?"), "/block/height/7");
    }

    /// Every one of these is a way a stranger could make the node do
    /// unbounded work, and every one of them is why HTTP is hand-rolled here.
    #[test]
    fn a_request_that_never_ends_is_refused_rather_than_buffered() {
        let (address, _node) = a_served_node();
        let mut client = std::net::TcpStream::connect(address).unwrap();

        // No newline, ever. A reader without a cap would take all of it.
        let flood = vec![b'A'; 64 * 1024];
        let _ = client.write_all(&flood);
        let _ = client.write_all(&flood);

        let answer = read_all(&mut client);

        assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
        assert!(answer.contains("request head is at most"), "{answer}");
    }

    #[test]
    fn a_body_past_the_bound_is_refused_before_it_is_read() {
        let (address, _node) = a_served_node();
        let mut client = std::net::TcpStream::connect(address).unwrap();

        client
            .write_all(
                format!(
                    "POST /tx HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
                    MAX_BODY + 1
                )
                .as_bytes(),
            )
            .unwrap();

        let answer = read_all(&mut client);

        assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
        assert!(answer.contains("body is at most"), "{answer}");
    }

    #[test]
    fn a_request_that_is_not_http_is_a_400_with_a_reason() {
        let (address, _node) = a_served_node();
        let mut client = std::net::TcpStream::connect(address).unwrap();
        client.write_all(b"this is not HTTP\r\n\r\n").unwrap();

        let answer = read_all(&mut client);

        assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
        assert!(answer.contains("application/json"), "{answer}");
        assert!(answer.contains("\"error\""), "{answer}");
    }

    /// The pool is fixed and the queue is bounded, so a stranger holding
    /// connections open costs the node a known number of threads and a known
    /// number of sockets — not one of each per connection.
    #[test]
    fn connections_past_the_queue_are_closed_rather_than_piling_up() {
        let (address, _node) = a_served_node();

        // Enough to fill the workers and the queue several times over, each
        // one silent so nothing is ever answered and nothing is ever freed.
        let mut held = Vec::new();
        for _ in 0..(WORKERS + WAITING) * 3 {
            if let Ok(stream) = std::net::TcpStream::connect(address) {
                held.push(stream);
            }
        }

        let mut refused = 0;
        for stream in &mut held {
            let mut said = [0u8; 32];
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            if stream.read(&mut said).unwrap_or(0) > 0 {
                refused += 1;
            }
        }

        assert!(
            refused > 0,
            "past the queue a connection must be answered and closed, not held"
        );
    }

    fn a_served_node() -> (SocketAddr, SharedNode) {
        let node = a_node();
        let listener = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = listener.local_addr().unwrap();
        let serving = Arc::clone(&node);
        thread::spawn(move || serve(listener, serving));

        (address, node)
    }

    fn read_all(client: &mut std::net::TcpStream) -> String {
        let mut answer = String::new();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let _ = client.read_to_string(&mut answer);
        answer
    }

    #[test]
    fn a_bound_server_answers_a_request_and_the_next_one() {
        let (address, _node) = a_served_node();

        for _ in 0..2 {
            let mut client = std::net::TcpStream::connect(address).unwrap();
            client
                .write_all(b"GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut answer = String::new();
            client.read_to_string(&mut answer).unwrap();

            assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
            assert!(answer.contains("application/json"), "{answer}");
            assert!(answer.contains("\"network\":\"test\""), "{answer}");
        }
    }

    #[test]
    fn a_request_for_nothing_gets_a_404_over_the_wire() {
        let (address, _node) = a_served_node();

        let mut client = std::net::TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();

        assert!(answer.starts_with("HTTP/1.1 404"), "{answer}");
    }

    #[test]
    fn a_taken_port_is_an_error_naming_the_address() {
        let first = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = first.local_addr().unwrap();

        let error = format!("{:#}", bind(address).unwrap_err());

        assert!(error.contains(&address.to_string()), "{error}");
    }
}
