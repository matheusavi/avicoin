use crate::node::{record, SharedNode};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Request, Response, Server, StatusCode};

/// How many requests are served at once. Fixed, like everything else a
/// stranger can reach: a thread per connection is a thread per stranger.
pub const WORKERS: usize = 4;

/// The largest response the API will send. A collection endpoint is capped
/// well below this; the ceiling is what stops a bug becoming a memory attack.
pub const MAX_RESPONSE: usize = 1 << 20;

/// Binding is the caller's, not this thread's: a port already taken must fail
/// the process, the same reason `main` binds the P2P listener itself.
pub fn bind(address: SocketAddr) -> Result<Server> {
    Server::http(address)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("could not serve the API on {address}"))
}

/// Serves until the process ends. Each worker takes the node lock, copies out
/// what it needs, and **releases it before writing a byte** — the rule
/// `node::record` follows for stdout and `broadcast` follows with `try_send`.
/// A client that stops reading must cost the peers nothing.
pub fn serve(server: Server, node: SharedNode) -> Result<()> {
    let server = Arc::new(server);

    let mut workers = Vec::new();
    for _ in 0..WORKERS {
        let server = Arc::clone(&server);
        let node = Arc::clone(&node);
        workers.push(thread::spawn(move || work(&server, &node)));
    }

    for worker in workers {
        let _ = worker.join();
    }

    Ok(())
}

fn work(server: &Server, node: &SharedNode) {
    while let Ok(request) = server.recv() {
        answer(request, node);
    }
}

/// Routes, then writes. The two halves are separate because the first needs
/// the node lock and the second must not have it.
fn answer(request: Request, node: &SharedNode) {
    // One handler's panic must not take a worker with it, the same way one
    // peer's panic does not take the listener down.
    let routed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        route(path(request.url()), node)
    }));

    let (status, body) = match routed {
        Ok(answer) => answer,
        Err(_) => {
            record(node, "API: a request panicked".to_string());
            (500, json!({"error": "the node could not answer that"}))
        }
    };

    let rendered = body.to_string();
    let response = if rendered.len() > MAX_RESPONSE {
        Response::from_string(json!({"error": "the response is too large"}).to_string())
            .with_status_code(StatusCode(500))
    } else {
        Response::from_string(rendered).with_status_code(StatusCode(status))
    };

    let _ = request.respond(response.with_header(json_header()));
}

fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("a constant header")
}

/// **Takes the node lock and gives it back before returning.** Nothing here
/// may borrow from the guard, because everything after this writes to a
/// socket a stranger controls the read end of.
fn route(path: &str, node: &SharedNode) -> (u16, Value) {
    match path {
        "/status" => (200, status(node)),
        _ => (404, json!({"error": "no such endpoint"})),
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

        let (status, body) = route("/status", &node);

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
    /// This is what the seam can prove. That the *write* happens after the
    /// routing is structural: `answer` calls `route`, takes its value, and
    /// only then touches the request.
    #[test]
    fn routing_gives_the_node_lock_back_before_it_returns() {
        let node = a_node();

        let _ = route("/status", &node);

        assert!(
            node.try_lock().is_ok(),
            "a handler must not hold the lock past its own return"
        );
    }

    #[test]
    fn an_unknown_path_is_a_404_with_a_reason() {
        let node = a_node();

        let (status, body) = route("/nothing-here", &node);

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

    #[test]
    fn a_bound_server_answers_a_request_and_the_next_one() {
        use std::io::{Read, Write};

        let node = a_node();
        let server = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.server_addr().to_ip().unwrap();
        thread::spawn(move || serve(server, node));

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
        use std::io::{Read, Write};

        let node = a_node();
        let server = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.server_addr().to_ip().unwrap();
        thread::spawn(move || serve(server, node));

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
        let address = first.server_addr().to_ip().unwrap();

        let error = match bind(address) {
            Ok(_) => panic!("a port already served must not be served twice"),
            Err(why) => format!("{why:#}"),
        };

        assert!(error.contains(&address.to_string()), "{error}");
    }
}
