use crate::address::Address;
use crate::amount::Amount;
use crate::block::BlockHash;
use crate::byte_reader::ByteReader;
use crate::messages::inventory::{Inventory, Item};
use crate::messages::message::Message;
use crate::node::{record, Handshake, Origin, SharedNode};
use crate::protocol::{accept_transaction, dial_requested, Dialled};
use crate::script::p2pkh;
use crate::transaction::{Transaction, Txid};
use crate::util::display_order;
use crate::validation::MAX_TRANSACTION_SIZE;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
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
    /// Where the page that made this request came from, and where it was
    /// sent. A browser sets `Origin` on every `POST`; the pair is what tells
    /// this node's own viewer from somebody else's page.
    pub origin: Option<String>,
    pub host: Option<String>,
}

impl Asked {
    /// Whether a `POST` came from this node's own page.
    ///
    /// A cross-origin `fetch` reaches a write endpoint without a preflight if
    /// its body is a simple content type, and the attacker never needs to see
    /// the response — the side effect *is* the attack. So a `POST` carrying an
    /// `Origin` that is not this node's is refused. A request with no `Origin`
    /// at all is a client that is not a browser, which is not what CSRF is.
    fn same_origin(&self) -> bool {
        let Some(origin) = &self.origin else {
            return true;
        };

        match (origin.split("//").nth(1), &self.host) {
            (Some(from), Some(host)) => from == host,
            _ => false,
        }
    }
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
            // The refusal came *while the client was still writing*, so
            // closing now makes the kernel send a reset and throw away the
            // answer it was about to read. A reader who is told nothing cannot
            // fix anything.
            part_politely(stream);
            return;
        }
    };

    if let Some((content_type, body)) = asset(path(&asked.url)) {
        let _ = match asked.method.as_str() {
            "GET" => send(stream, 200, content_type, body.as_bytes()),
            // A HEAD answer carries no body, and a server that answers GET
            // answers HEAD.
            "HEAD" => send(stream, 200, content_type, b""),
            _ => write(stream, 405, &json!({"error": "the viewer is read-only"})),
        };
        return;
    }

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

/// Says "no more from us", then reads what is still coming until the client
/// gives up on it. Bounded twice over — by `PARTING` and by what the buffer
/// holds — because this is the path a stranger reaches by sending too much,
/// and waiting on them is what a bad request must not be able to make us do.
fn part_politely(stream: &mut TcpStream) {
    const PARTING: Duration = Duration::from_millis(250);

    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(PARTING));

    let deadline = std::time::Instant::now() + PARTING;
    let mut discarded = [0u8; 8 * 1024];
    while std::time::Instant::now() < deadline {
        match stream.read(&mut discarded) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
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

    send(stream, status, "application/json", rendered.as_bytes())
}

fn send(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );

    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
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

    let headers: Vec<(&str, &str)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim(), value.trim()))
        .collect();
    let header = |wanted: &str| {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.to_string())
    };

    let length = header("content-length")
        .map(|value| value.parse::<usize>())
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
        origin: header("origin"),
        host: header("host"),
        body,
    })
}

/// **Takes the node lock and gives it back before returning.** Nothing here
/// may borrow from the guard, because everything after this writes to a
/// socket a stranger controls the read end of.
fn route(asked: &Asked, node: &SharedNode) -> (u16, Value) {
    let segments: Vec<&str> = path(&asked.url).split('/').collect();
    let query = query(&asked.url);

    if asked.method == "POST" && !asked.same_origin() {
        return (
            403,
            json!({"error": "a write has to come from this node's own page"}),
        );
    }

    match (asked.method.as_str(), segments.as_slice()) {
        ("GET", ["", "status"]) => (200, status(node)),
        ("GET", ["", "blocks"]) => blocks(node, &query),
        ("GET", ["", "block", "height", height]) => by_height(node, height),
        ("GET", ["", "block", hash]) => by_hash(node, hash),
        ("GET", ["", "tx", txid]) => transaction(node, txid),
        ("GET", ["", "address", address]) => holdings(node, address, &query),
        ("GET", ["", "mempool"]) => (200, mempool(node)),
        ("GET", ["", "peers"]) => (200, peers(node)),
        ("GET", ["", "log"]) => log(node, &query),
        ("POST", ["", "tx"]) => submit(node, &asked.body),
        ("POST", ["", "connect"]) => connect(node, &asked.body),
        ("GET", _) => (404, json!({"error": "no such endpoint"})),
        // The viewer is answered before this in `answer`, since it is the one
        // thing here that is not JSON.
        (method, _) => (
            405,
            json!({ "error": format!("{method} is not something this endpoint answers") }),
        ),
    }
}

/// The viewer, compiled in.
///
/// `include_str!` rather than a directory to read from: it keeps the
/// deployment one artefact with nothing to lose beside it, and the bytes
/// served are byte-for-byte the files in the repo. There is no build step —
/// no bundler, no transpiler, no external request — which is the acceptance
/// criterion, not "the files are read at runtime".
fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
        "/" | "/index.html" => Some((
            "text/html; charset=utf-8",
            include_str!("viewer/index.html"),
        )),
        "/viewer.css" => Some(("text/css; charset=utf-8", include_str!("viewer/viewer.css"))),
        "/viewer.js" => Some((
            "text/javascript; charset=utf-8",
            include_str!("viewer/viewer.js"),
        )),
        _ => None,
    }
}

/// The most blocks one `/blocks` request will describe. A page, not a chain:
/// asking for the whole history in one response is the memory attack every
/// other bound here exists to prevent.
pub const MAX_PAGE: usize = 50;

fn missing(what: &str) -> (u16, Value) {
    (404, json!({ "error": format!("no such {what}") }))
}

fn malformed(why: impl std::fmt::Display) -> (u16, Value) {
    (400, json!({ "error": why.to_string() }))
}

/// Big-endian, the way an explorer shows a hash and the way the node's own log
/// prints one — reversed from the bytes anything hashes. Invariant 5 puts this
/// conversion at the edge and nowhere else.
fn from_display(text: &str) -> Result<[u8; 32], String> {
    let mut bytes: [u8; 32] = hex::decode(text)
        .map_err(|_| format!("{text:?} is not hexadecimal"))?
        .try_into()
        .map_err(|_| format!("{text:?} is not a 32-byte hash"))?;
    bytes.reverse();

    Ok(bytes)
}

fn by_hash(node: &SharedNode, text: &str) -> (u16, Value) {
    let hash = match from_display(text) {
        Ok(bytes) => BlockHash::from_bytes(bytes),
        Err(why) => return malformed(why),
    };

    match described(node, &hash) {
        Some(block) => (200, block),
        None => missing("block"),
    }
}

fn by_height(node: &SharedNode, text: &str) -> (u16, Value) {
    let height: usize = match text.parse() {
        Ok(height) => height,
        Err(_) => return malformed(format!("{text:?} is not a height")),
    };

    let hash = {
        let held = node.lock().expect("node lock poisoned");
        held.chain.index().best_chain().get(height).copied()
    };

    match hash.and_then(|hash| described(node, &hash)) {
        Some(block) => (200, block),
        None => missing("block"),
    }
}

/// One block, as much of it as the node holds. The body may only be on disk,
/// so this is where the two halves are put back together.
fn described(node: &SharedNode, hash: &BlockHash) -> Option<Value> {
    let (entry, body, files, tip, on_best) = {
        let held = node.lock().expect("node lock poisoned");
        let entry = held.chain.index().get(hash)?.clone();
        (
            entry,
            held.chain.cached_body(hash),
            held.chain.files(),
            held.chain.height(),
            // The *connected* chain, not the header chain. A body can be held
            // above the connected tip — two peers answering `getdata` at once
            // is enough — and calling that one confirmed would give it a
            // negative count.
            held.chain
                .index()
                .height_on_best(hash)
                .is_some_and(|height| height as u32 <= held.chain.height()),
        )
    };

    // Off the lock, because a block read from `blocks.dat` is a seek and a
    // parse and a stranger picks which one.
    let body = body.or_else(|| files?.block(hash).ok().flatten())?;
    let raw = body.get_raw_format().ok()?;

    // Zero off the best chain. A block on a branch that lost is not confirmed
    // by anything, and giving it the same number as the block that beat it
    // would be saying it was.
    let confirmations = if on_best {
        (tip as i64 - entry.height as i64) + 1
    } else {
        0
    };

    Some(json!({
        "hash": hash.to_string(),
        "height": entry.height,
        "best_chain": on_best,
        "confirmations": confirmations,
        "version": entry.header.version,
        "previous_block": entry.header.previous_block_hash.to_string(),
        "merkle_root": hex::encode(display_order(entry.header.merkle_root)),
        "time": entry.header.time,
        "n_bits": format!("{:#010x}", entry.header.n_bits),
        "nonce": entry.header.nonce,
        "size": raw.len(),
        "transaction_count": body.transactions.len(),
        // Capped like every other collection. A megabyte block renders to
        // several megabytes of JSON, and a response that became a 500 for
        // being too large would be worse than one that says how much it left
        // out.
        "transactions": body
            .transactions
            .iter()
            .take(MAX_LISTED)
            .map(rendered)
            .collect::<Vec<_>>(),
    }))
}

fn blocks(node: &SharedNode, query: &HashMap<String, String>) -> (u16, Value) {
    let from: usize = match query.get("from").map(|value| value.parse()) {
        Some(Ok(from)) => from,
        Some(Err(_)) => return malformed("from is not a height"),
        None => 0,
    };
    let count: usize = match query.get("count").map(|value| value.parse()) {
        Some(Ok(count)) => count,
        Some(Err(_)) => return malformed("count is not a number"),
        None => MAX_PAGE,
    };

    if count > MAX_PAGE {
        return malformed(format!("count is at most {MAX_PAGE}"));
    }

    let held = node.lock().expect("node lock poisoned");
    // The *connected* chain, not the header chain. Headers run ahead of
    // bodies during sync, and a page listing blocks `/block/height` answers
    // 404 for would be a page of things this node does not have.
    let tip = held.chain.height() as usize;
    let page: Vec<Value> = held
        .chain
        .index()
        .best_chain()
        .iter()
        .enumerate()
        .take(tip + 1)
        .skip(from)
        .take(count)
        .filter_map(|(height, hash)| {
            let entry = held.chain.index().get(hash)?;
            Some(json!({
                "hash": hash.to_string(),
                "height": height,
                "time": entry.header.time,
            }))
        })
        .collect();

    (200, json!({"height": tip, "blocks": page}))
}

fn transaction(node: &SharedNode, text: &str) -> (u16, Value) {
    let txid = match from_display(text) {
        Ok(bytes) => Txid::from_bytes(bytes),
        Err(why) => return malformed(why),
    };

    let (held_by_mempool, chain, files) = {
        let held = node.lock().expect("node lock poisoned");
        // Connected, not merely known: the header chain runs ahead of the
        // bodies, and a window of five hundred header-only entries would
        // report "not in the last 500 blocks" for a transaction on disk.
        let connected = held.chain.height() as usize + 1;
        (
            held.mempool.get(&txid).cloned(),
            held.chain.index().best_chain()[..connected].to_vec(),
            held.chain.files(),
        )
    };

    if let Some(transaction) = held_by_mempool {
        let mut value = rendered(&transaction);
        value["confirmations"] = json!(0);
        return (200, value);
    }

    // Newest first and bounded: nothing indexes a transaction by its id, so
    // this is a scan, and an unknown txid would otherwise walk — and re-parse
    // off disk — the whole chain for any stranger who asked.
    let searched: Vec<(usize, &BlockHash)> =
        chain.iter().enumerate().rev().take(MAX_SCANNED).collect();

    for (height, hash) in searched {
        let body = {
            let held = node.lock().expect("node lock poisoned");
            held.chain.cached_body(hash)
        }
        .or_else(|| files.as_ref()?.block(hash).ok().flatten());

        let Some(body) = body else { continue };
        if let Some(found) = body
            .transactions
            .iter()
            .find(|candidate| candidate.get_tx_id() == txid)
        {
            let mut value = rendered(found);
            value["block"] = json!(hash.to_string());
            value["height"] = json!(height);
            return (200, value);
        }
    }

    (
        404,
        json!({"error": format!(
            "no such transaction in the mempool or the last {MAX_SCANNED} blocks; \
             this node does not index transactions by id"
        )}),
    )
}

/// Both ids, because witness separation ([ADR-0003](../docs/adr/0003-transaction-witness-format.md))
/// is a thing a reader should be able to see rather than take on trust.
fn rendered(transaction: &Transaction) -> Value {
    json!({
        "txid": transaction.get_tx_id().to_string(),
        "wtxid": transaction.get_wtxid().to_string(),
        "version": transaction.version,
        "coinbase": transaction.is_coinbase(),
        "size": transaction.get_raw_format().len(),
        "inputs": transaction
            .inputs
            .iter()
            .map(|input| json!({
                "previous_output": {
                    "txid": input.previous_output.txid.to_string(),
                    "index": input.previous_output.v_out,
                },
                "witness_items": input.witness.items().len(),
            }))
            .collect::<Vec<_>>(),
        "outputs": transaction
            .outputs
            .iter()
            .enumerate()
            .map(|(index, output)| json!({
                "index": index,
                "atoms": output.value.atoms(),
                "avi": in_avi(output.value),
                "script_pubkey": hex::encode(&output.script_pubkey),
            }))
            .collect::<Vec<_>>(),
    })
}

/// The division lives on `Amount`, so this and `Display` cannot drift apart.
fn in_avi(amount: Amount) -> String {
    amount.in_avi()
}

/// A **signed** transaction, as hex, through the same path a peer's `tx`
/// message takes: the same validation, the same mempool, the same relay.
///
/// There is no second door. A rule enforced for a stranger and not for a
/// `POST` would be a rule with a hole in it, so `accept_transaction` is the
/// one way in and this calls it. The API never signs — a public URL must not
/// be able to spend the operator's coins.
fn submit(node: &SharedNode, body: &[u8]) -> (u16, Value) {
    let text = match std::str::from_utf8(body) {
        Ok(text) => text.trim(),
        Err(_) => return malformed("a transaction is hex, and this is not text"),
    };

    let raw = match hex::decode(text) {
        Ok(raw) => raw,
        Err(why) => return malformed(format!("a transaction is hex: {why}")),
    };

    if raw.len() > MAX_TRANSACTION_SIZE {
        return malformed(format!(
            "a transaction is at most {MAX_TRANSACTION_SIZE} bytes"
        ));
    }

    let transaction = match Transaction::parse_raw(&mut ByteReader::new(&raw)) {
        Ok(transaction) => transaction,
        Err(why) => return malformed(format!("that is not a transaction: {why:#}")),
    };

    let txid = match accept_transaction(node, transaction) {
        Ok(txid) => txid,
        // The reason, not just a refusal. A demo where a submission fails
        // silently is worse than one where it fails.
        Err(why) => return (400, json!({ "error": format!("{why:#}") })),
    };

    relay(node, txid);

    (200, json!({"txid": txid.to_string()}))
}

/// To every Ready peer. There is nobody to leave out — this did not come from
/// one of them.
fn relay(node: &SharedNode, txid: Txid) {
    let network = node.lock().expect("node lock poisoned").config.network;
    let Ok(offer) = Message::new(Inventory::offered(vec![Item::Transaction(txid)]), network)
        .and_then(|message| message.get_raw_format())
    else {
        return;
    };

    node.lock()
        .expect("node lock poisoned")
        .peers
        .relay(&offer, None);
}

/// Dials an address through the same path a configured peer takes, budget and
/// caps included. This is not a way around a limit the P2P layer enforces.
fn connect(node: &SharedNode, body: &[u8]) -> (u16, Value) {
    let text = match std::str::from_utf8(body) {
        Ok(text) => text.trim(),
        Err(_) => return malformed("an address is text, and this is not"),
    };

    let address: SocketAddr = match text.parse() {
        Ok(address) => address,
        Err(_) => return malformed(format!("{text:?} is not an address (expected host:port)")),
    };

    match dial_requested(address, node) {
        Dialled::Started => (200, json!({"dialling": address.to_string()})),
        refused => (
            400,
            json!({ "error": format!("{address} was not dialled: {refused}") }),
        ),
    }
}

/// How many of a collection one response describes.
pub const MAX_LISTED: usize = 200;

/// How far back `/tx` looks. Nothing indexes a transaction by its id, so it is
/// a scan — and a scan a stranger picks the cost of is one that needs an end.
pub const MAX_SCANNED: usize = 500;

/// The balance and unspent outputs of one address.
///
/// Both come from the UTXO set rather than from a walk over blocks: the set is
/// what "unspent" means, and a scan of the chain would be answering a
/// different question slowly.
fn holdings(node: &SharedNode, text: &str, query: &HashMap<String, String>) -> (u16, Value) {
    let address: Address = match text.parse() {
        Ok(address) => address,
        Err(why) => return malformed(format!("{text:?} is not an address: {why:#}")),
    };

    let from: usize = match query.get("from").map(|value| value.parse()) {
        Some(Ok(from)) => from,
        Some(Err(_)) => return malformed("from is not a number"),
        None => 0,
    };

    let script = p2pkh(&address.pubkey_hash());
    let (coins, atoms, total) = {
        let held = node.lock().expect("node lock poisoned");
        held.utxo.paying(&script, from, MAX_LISTED)
    };

    let unspent: Vec<Value> = coins
        .iter()
        .map(|(outpoint, coin)| {
            json!({
                "txid": outpoint.txid.to_string(),
                "index": outpoint.v_out,
                "atoms": coin.output.value.atoms(),
                "avi": in_avi(coin.output.value),
                "height": coin.height,
                "coinbase": coin.from_coinbase,
            })
        })
        .collect();

    let balance = match Amount::from_atoms(atoms) {
        Ok(balance) => balance,
        Err(_) => return (500, json!({"error": "this address holds past MAX_MONEY"})),
    };

    (
        200,
        json!({
            "address": text,
            "atoms": balance.atoms(),
            "avi": in_avi(balance),
            "unspent_count": total,
            "unspent": unspent,
        }),
    )
}

/// What is pending, richest first — the order a miner would take them in.
fn mempool(node: &SharedNode) -> Value {
    let (total, entries) = {
        let held = node.lock().expect("node lock poisoned");
        (held.mempool.len(), held.mempool.richest(MAX_LISTED))
    };

    json!({
        "count": total,
        "transactions": entries
            .iter()
            .map(|entry| json!({
                "txid": entry.transaction.get_tx_id().to_string(),
                "fee_atoms": entry.fee.atoms(),
                "size": entry.transaction.get_raw_format().len(),
            }))
            .collect::<Vec<_>>(),
    })
}

/// Where a peer **listens**, never the ephemeral source port an accepted
/// connection came from — that is not an address anyone could dial back
/// ([ADR-0015](../docs/adr/0015-peer-identity-and-duplicate-connections.md)).
fn peers(node: &SharedNode) -> Value {
    let held = node.lock().expect("node lock poisoned");
    let since = crate::util::now();
    let mut listed: Vec<Value> = held
        .peers
        .all()
        .iter()
        .map(|(id, peer)| {
            json!({
                "id": id,
                "listening": peer.listening.map(|address: std::net::SocketAddr| address.to_string()),
                "direction": match peer.origin {
                    Origin::Dialled => "outbound",
                    Origin::Accepted => "inbound",
                },
                "handshake": match peer.handshake {
                    Handshake::AwaitingVersion => "awaiting-version",
                    Handshake::AwaitingVerack => "awaiting-verack",
                    Handshake::Ready => "ready",
                },
                "connected_seconds": since.saturating_sub(peer.connected_at),
            })
        })
        .collect();
    // Sorted before it is truncated, so the same table gives the same answer
    // twice — `MAX_PEERS` is 32, so nothing is ever cut, but the order is not
    // the table's to choose.
    listed.sort_by_key(|peer| peer["id"].as_u64().unwrap_or_default());
    listed.truncate(MAX_LISTED);

    json!({"count": held.peers.len(), "peers": listed})
}

/// The tail of the bounded `Log`, which was built for a reader in M1 and has
/// not had one until now.
fn log(node: &SharedNode, query: &HashMap<String, String>) -> (u16, Value) {
    let since: usize = match query.get("since").map(|value| value.parse()) {
        Some(Ok(since)) => since,
        Some(Err(_)) => return malformed("since is not a number"),
        None => 0,
    };

    let held = node.lock().expect("node lock poisoned");
    let (next, lines) = held.log.tail(since, MAX_LISTED);

    (200, json!({"next": next, "lines": lines}))
}

fn query(url: &str) -> HashMap<String, String> {
    url.split_once('?')
        .map(|(_, rest)| rest)
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
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
        "headers": held.chain.index().len(),
        "coins": held.utxo.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::ATOMS_PER_AVI;
    use crate::config::Config;
    use crate::node::Node;
    use crate::params::TESTNET;
    use crate::wallet::Wallet;
    use rstest::rstest;

    fn get(url: &str) -> Asked {
        Asked {
            method: "GET".to_string(),
            origin: None,
            host: None,
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
        assert_eq!(body["headers"], 1, "genesis, and nothing else yet");
        assert_eq!(body["coins"], 3, "the test allocation");
    }

    /// `headers` counts what the index knows and `height` what the chain has
    /// connected. They part company the moment a header arrives without its
    /// body, which is the ordinary state of a syncing node.
    #[test]
    fn status_counts_headers_and_coins_apart_from_the_connected_height() {
        let (node, block) = a_mined_node();

        let (_, body) = route(&get("/status"), &node);

        assert_eq!(body["height"], 1);
        assert_eq!(body["headers"], 2, "genesis and the block just mined");
        assert_eq!(
            body["coins"],
            3 + block.transactions.len(),
            "the allocation, plus what the block paid"
        );
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

    fn post(url: &str, body: &str) -> Asked {
        Asked {
            method: "POST".to_string(),
            origin: None,
            host: None,
            url: url.to_string(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_signed_transaction_posted_as_hex_reaches_the_mempool() {
        use crate::crypto::PrivateKey;
        use crate::validation::fixtures::{funded, pay_to, signed};

        let node = a_node();
        let key = PrivateKey::random();
        let payment = {
            let mut held = node.lock().unwrap();
            let outpoint = funded(&mut held.utxo, &key, 1_000, 0);
            signed(&key, &[outpoint], vec![pay_to(&key, 900)])
        };

        let (status, body) = route(&post("/tx", &hex::encode(payment.get_raw_format())), &node);

        assert_eq!(status, 200, "{body}");
        assert_eq!(body["txid"], payment.get_tx_id().to_string());
        assert_eq!(node.lock().unwrap().mempool.len(), 1);
    }

    /// The same rules a peer's `tx` meets, because it is the same call. A rule
    /// enforced for a stranger and not here would be a rule with a hole in it.
    #[test]
    fn a_transaction_the_p2p_path_refuses_is_refused_here_for_the_same_reason() {
        use crate::crypto::PrivateKey;
        use crate::validation::fixtures::{funded, pay_to, signed};

        let node = a_node();
        let key = PrivateKey::random();
        let forged = {
            let mut held = node.lock().unwrap();
            let outpoint = funded(&mut held.utxo, &key, 1_000, 0);
            // Paying out more than it takes in: the same refusal `check_spend`
            // makes for a peer.
            signed(&key, &[outpoint], vec![pay_to(&key, 5_000)])
        };
        let over_the_wire = accept_transaction(&node, forged.clone())
            .map(|txid| txid.to_string())
            .unwrap_err()
            .to_string();

        let (status, body) = route(&post("/tx", &hex::encode(forged.get_raw_format())), &node);

        assert_eq!(status, 400);
        assert!(
            body["error"].as_str().unwrap().contains(&over_the_wire),
            "{body} against {over_the_wire}"
        );
        assert_eq!(node.lock().unwrap().mempool.len(), 0, "and nothing is held");
    }

    #[rstest]
    #[case::not_hex("this is not hex")]
    #[case::hex_that_is_not_a_transaction("deadbeef")]
    #[case::empty("")]
    fn a_body_that_is_not_a_transaction_is_a_400(#[case] body: &str) {
        let node = a_node();

        let (status, answer) = route(&post("/tx", body), &node);

        assert_eq!(status, 400, "{body:?}: {answer}");
        assert!(answer["error"].is_string());
        assert_eq!(node.lock().unwrap().mempool.len(), 0);
    }

    #[test]
    fn a_transaction_past_the_consensus_bound_is_refused_before_it_is_parsed() {
        let node = a_node();
        let fat = hex::encode(vec![0u8; MAX_TRANSACTION_SIZE + 1]);

        let (status, body) = route(&post("/tx", &fat), &node);

        assert_eq!(status, 400);
        assert!(
            body["error"].as_str().unwrap().contains("at most"),
            "{body}"
        );
    }

    #[test]
    fn connecting_to_this_nodes_own_address_is_refused_with_a_reason() {
        let node = a_node();
        let ours = node.lock().unwrap().config.host_address;

        let (status, body) = route(&post("/connect", &ours.to_string()), &node);

        assert_eq!(status, 400);
        assert!(
            body["error"].as_str().unwrap().contains("own address"),
            "{body}"
        );
    }

    #[test]
    fn connecting_to_an_address_that_is_already_a_peer_is_refused() {
        let node = a_node();
        let peer: SocketAddr = "127.0.0.1:5999".parse().unwrap();
        {
            let mut held = node.lock().unwrap();
            held.peers
                .register(
                    peer,
                    Origin::Dialled,
                    std::sync::mpsc::sync_channel(1).0,
                    Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                )
                .unwrap();
        }

        let (status, body) = route(&post("/connect", &peer.to_string()), &node);

        assert_eq!(status, 400);
        assert!(
            body["error"].as_str().unwrap().contains("already a peer"),
            "{body}"
        );
    }

    #[test]
    fn connecting_past_the_peer_cap_is_refused() {
        let node = a_node();
        {
            let mut held = node.lock().unwrap();
            for port in 0..crate::node::MAX_PEERS {
                let address: SocketAddr = format!("127.0.0.1:{}", 6000 + port).parse().unwrap();
                held.peers
                    .register(
                        address,
                        Origin::Dialled,
                        std::sync::mpsc::sync_channel(1).0,
                        Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    )
                    .unwrap();
            }
        }

        let (status, body) = route(&post("/connect", "127.0.0.1:7000"), &node);

        assert_eq!(status, 400);
        assert!(body["error"].as_str().unwrap().contains("full"), "{body}");
    }

    #[test]
    fn an_address_that_is_not_an_address_is_a_400_rather_than_a_dial() {
        let node = a_node();

        for text in ["8080", "not-an-address", ""] {
            let (status, body) = route(&post("/connect", text), &node);

            assert_eq!(status, 400, "{text:?}: {body}");
        }
    }

    #[test]
    fn a_get_on_a_write_endpoint_is_a_404_and_a_post_on_a_read_one_is_a_405() {
        let node = a_node();

        assert_eq!(route(&get("/tx"), &node).0, 404);
        assert_eq!(route(&post("/status", ""), &node).0, 405);
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

    fn a_mined_node() -> (SharedNode, crate::block::Block) {
        use crate::transaction::TxOut;

        let node = a_node();
        let block = {
            let mut held = node.lock().unwrap();
            let parent = held.chain.tip();
            let n_bits = held
                .chain
                .index()
                .required_bits_after(&parent, &TESTNET)
                .unwrap();
            let coinbase = Transaction::coinbase(
                1,
                0,
                vec![TxOut {
                    value: crate::amount::subsidy(1),
                    script_pubkey: vec![0x51],
                }],
            );
            let mut block = crate::block::Block::new(
                1,
                *parent.as_bytes(),
                TESTNET.genesis_time + 1,
                n_bits,
                vec![coinbase],
            );
            block.nonce = block.search(0, u32::MAX).unwrap();
            block.seal().unwrap();

            let crate::node::Node {
                chain,
                utxo,
                mempool,
                ..
            } = &mut *held;
            chain
                .accept(
                    block.clone(),
                    utxo,
                    mempool,
                    TESTNET.genesis_time + 2,
                    &TESTNET,
                )
                .unwrap();
            block
        };

        (node, block)
    }

    #[test]
    fn a_block_is_served_by_hash_and_by_height_and_the_two_agree() {
        let (node, block) = a_mined_node();
        let hash = block.header().unwrap().hash();

        let (by_hash_status, by_hash_body) = route(&get(&format!("/block/{hash}")), &node);
        let (by_height_status, by_height_body) = route(&get("/block/height/1"), &node);

        assert_eq!((by_hash_status, by_height_status), (200, 200));
        assert_eq!(by_hash_body, by_height_body);
        assert_eq!(by_hash_body["height"], 1);
        assert_eq!(by_hash_body["confirmations"], 1);
        assert_eq!(by_hash_body["size"], block.get_raw_format().unwrap().len());
    }

    /// Invariant 5: a hash is reversed only where a person reads it. The
    /// display form must not be what anything hashes, so the one a request
    /// carries has to be reversed back before it is looked up.
    #[test]
    fn a_hash_in_a_response_is_big_endian_and_is_not_what_was_hashed() {
        let (node, block) = a_mined_node();
        let header = block.header().unwrap();
        let hash = header.hash();

        let (_, body) = route(&get(&format!("/block/{hash}")), &node);

        assert_eq!(body["hash"], hash.to_string());
        assert_ne!(body["hash"], hex::encode(hash.as_bytes()));
        assert_eq!(
            body["hash"].as_str().unwrap(),
            hex::encode(display_order(*hash.as_bytes()))
        );
    }

    #[test]
    fn a_transaction_carries_both_its_ids_and_they_differ_when_a_witness_does() {
        let (node, block) = a_mined_node();
        let coinbase = &block.transactions[0];

        let (status, body) = route(&get(&format!("/tx/{}", coinbase.get_tx_id())), &node);

        assert_eq!(status, 200);
        assert_eq!(body["txid"], coinbase.get_tx_id().to_string());
        assert_eq!(body["wtxid"], coinbase.get_wtxid().to_string());
        assert_eq!(body["height"], 1);
        assert!(body["coinbase"].as_bool().unwrap());
    }

    /// ADR-0003 separates the two ids, and a reader should be able to see
    /// that rather than take it on trust. A witnessed transaction's `wtxid`
    /// covers bytes its `txid` does not, so they differ — and neither is the
    /// other reversed, which is the mistake a display-order bug would make.
    #[test]
    fn a_witnessed_transactions_two_ids_differ_and_neither_is_the_other_reversed() {
        use crate::crypto::PrivateKey;
        use crate::validation::fixtures::{funded, pay_to, signed};

        let node = a_node();
        let key = PrivateKey::random();
        let payment = {
            let mut held = node.lock().unwrap();
            let outpoint = funded(&mut held.utxo, &key, 1_000, 0);
            let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
            let crate::node::Node { mempool, utxo, .. } = &mut *held;
            mempool.accept(payment.clone(), utxo, 1, &TESTNET).unwrap();
            payment
        };

        let (status, body) = route(&get(&format!("/tx/{}", payment.get_tx_id())), &node);

        assert_eq!(status, 200);
        assert_ne!(body["txid"], body["wtxid"], "a witness has to show");
        assert_eq!(body["confirmations"], 0, "it is in the mempool");
        assert_eq!(body["inputs"][0]["witness_items"], 2);
        assert_ne!(
            body["txid"].as_str().unwrap(),
            hex::encode(payment.get_wtxid().as_bytes()),
            "neither id is the other in the wrong byte order"
        );
    }

    #[test]
    fn an_amount_renders_in_avi_without_losing_an_atom() {
        assert_eq!(in_avi(Amount::from_atoms(0).unwrap()), "0.00000000");
        assert_eq!(in_avi(Amount::from_atoms(1).unwrap()), "0.00000001");
        assert_eq!(
            in_avi(Amount::from_atoms(ATOMS_PER_AVI).unwrap()),
            "1.00000000"
        );
        assert_eq!(
            in_avi(Amount::from_atoms(5_000_000_099).unwrap()),
            "50.00000099"
        );
    }

    #[test]
    fn an_output_carries_the_atoms_the_avi_string_is_made_from() {
        let (node, block) = a_mined_node();
        let coinbase = &block.transactions[0];

        let (_, body) = route(&get(&format!("/tx/{}", coinbase.get_tx_id())), &node);
        let output = &body["outputs"][0];

        assert_eq!(output["atoms"], coinbase.outputs[0].value.atoms());
        assert_eq!(output["avi"], in_avi(coinbase.outputs[0].value));
    }

    #[rstest]
    #[case::unknown_hash(&format!("/block/{}", "11".repeat(32)), 404)]
    #[case::height_past_the_tip("/block/height/9999", 404)]
    #[case::unknown_txid(&format!("/tx/{}", "22".repeat(32)), 404)]
    #[case::hash_that_is_not_hex("/block/not-a-hash", 400)]
    #[case::hash_of_the_wrong_length("/block/abcd", 400)]
    #[case::height_that_is_not_a_number("/block/height/seven", 400)]
    #[case::negative_height("/block/height/-1", 400)]
    #[case::count_that_is_not_a_number("/blocks?count=lots", 400)]
    #[case::count_past_the_cap("/blocks?count=10000", 400)]
    fn a_request_that_cannot_be_answered_says_which_kind_of_wrong_it_is(
        #[case] path: &str,
        #[case] expected: u16,
    ) {
        let (node, _) = a_mined_node();

        let (status, body) = route(&get(path), &node);

        assert_eq!(status, expected, "{path}: {body}");
        assert!(body["error"].is_string(), "{path}: {body}");
    }

    #[test]
    fn a_page_of_blocks_is_capped_and_starts_where_it_was_asked_to() {
        let (node, block) = a_mined_node();

        let (status, body) = route(&get("/blocks?from=1&count=1"), &node);

        assert_eq!(status, 200);
        assert_eq!(body["height"], 1);
        assert_eq!(body["blocks"].as_array().unwrap().len(), 1);
        assert_eq!(
            body["blocks"][0]["hash"],
            block.header().unwrap().hash().to_string()
        );
    }

    #[test]
    fn an_address_balance_is_the_sum_of_its_unspent_outputs() {
        let node = a_node();
        let address = node.lock().unwrap().wallet.address().to_string();
        let hash = node.lock().unwrap().wallet.pubkey_hash();
        {
            let mut held = node.lock().unwrap();
            let key = crate::crypto::PrivateKey::random();
            crate::validation::fixtures::funded(&mut held.utxo, &key, 500, 0);
        }
        // Two coins the wallet does own, and one it does not.
        let expected = {
            let mut held = node.lock().unwrap();
            let mut total = 0;
            for (index, atoms) in [(7u32, 1_000u64), (8, 2_500)] {
                held.utxo
                    .connect(
                        &Transaction::coinbase(
                            index,
                            0,
                            vec![crate::transaction::TxOut {
                                value: Amount::from_atoms(atoms).unwrap(),
                                script_pubkey: p2pkh(&hash),
                            }],
                        ),
                        index,
                    )
                    .unwrap();
                total += atoms;
            }
            total
        };

        let (status, body) = route(&get(&format!("/address/{address}")), &node);

        assert_eq!(status, 200);
        assert_eq!(body["atoms"], expected);
        assert_eq!(body["avi"], in_avi(Amount::from_atoms(expected).unwrap()));
        let unspent = body["unspent"].as_array().unwrap();
        assert_eq!(unspent.len(), 2);
        assert_eq!(
            unspent
                .iter()
                .map(|c| c["atoms"].as_u64().unwrap())
                .sum::<u64>(),
            expected
        );
    }

    /// 200 with nothing, not 404: an address nobody has paid is a real address
    /// with no coins, and a caller must be able to tell that from a typo.
    #[test]
    fn an_address_with_no_coins_is_answered_rather_than_missing() {
        let node = a_node();
        let unpaid = crate::address::Address::for_pubkey_hash(
            crate::crypto::PubKeyHash::from_bytes([7; 20]),
        );

        let (status, body) = route(&get(&format!("/address/{unpaid}")), &node);

        assert_eq!(status, 200);
        assert_eq!(body["atoms"], 0);
        assert!(body["unspent"].as_array().unwrap().is_empty());
    }

    #[test]
    fn an_address_that_is_not_an_address_is_a_400() {
        let node = a_node();

        for text in ["not-base58check", "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2"] {
            let (status, body) = route(&get(&format!("/address/{text}")), &node);

            assert_eq!(status, 400, "{text}: {body}");
            assert!(body["error"].is_string());
        }
    }

    #[test]
    fn the_mempool_is_served_richest_first_and_empties_when_it_is_emptied() {
        use crate::crypto::PrivateKey;
        use crate::validation::fixtures::{funded, pay_to, signed};

        let node = a_node();
        let key = PrivateKey::random();
        let txid = {
            let mut held = node.lock().unwrap();
            let outpoint = funded(&mut held.utxo, &key, 1_000, 0);
            let payment = signed(&key, &[outpoint], vec![pay_to(&key, 900)]);
            let crate::node::Node { mempool, utxo, .. } = &mut *held;
            mempool.accept(payment, utxo, 1, &TESTNET).unwrap()
        };

        let (status, body) = route(&get("/mempool"), &node);

        assert_eq!(status, 200);
        assert_eq!(body["count"], 1);
        assert_eq!(body["transactions"][0]["txid"], txid.to_string());
        assert_eq!(body["transactions"][0]["fee_atoms"], 100);

        node.lock().unwrap().mempool.remove(&txid);
        assert_eq!(route(&get("/mempool"), &node).1["count"], 0);
    }

    /// The listening address, never `PeerHandle.address` — that is an
    /// ephemeral source port on anything we accepted, and nobody could dial it.
    #[test]
    fn a_peer_is_reported_by_where_it_listens() {
        let node = a_node();
        let source: std::net::SocketAddr = "127.0.0.1:54321".parse().unwrap();
        let listens: std::net::SocketAddr = "127.0.0.1:34352".parse().unwrap();
        {
            let mut held = node.lock().unwrap();
            let id = held
                .peers
                .register(
                    source,
                    Origin::Accepted,
                    std::sync::mpsc::sync_channel(1).0,
                    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                )
                .unwrap();
            held.identify(id, 9, listens);
        }

        let (status, body) = route(&get("/peers"), &node);

        assert_eq!(status, 200);
        assert_eq!(body["count"], 1);
        assert_eq!(body["peers"][0]["listening"], listens.to_string());
        assert_ne!(body["peers"][0]["listening"], source.to_string());
        assert_eq!(body["peers"][0]["direction"], "inbound");
    }

    #[test]
    fn the_log_is_served_and_since_returns_only_what_followed() {
        let node = a_node();
        for line in ["first", "second", "third"] {
            record(&node, line.to_string());
        }

        let (status, all) = route(&get("/log"), &node);
        let (_, after) = route(
            &get(&format!("/log?since={}", all["next"].as_u64().unwrap() - 1)),
            &node,
        );

        assert_eq!(status, 200);
        assert_eq!(all["lines"].as_array().unwrap().len(), 3);
        assert_eq!(after["lines"], json!(["third"]));
        assert_eq!(route(&get("/log?since=lots"), &node).0, 400);
    }

    /// On the key's **actual bytes**, in both orders, across every endpoint
    /// including the two that render scripts. Asserting that no response
    /// contains the word "key" would only ever find `script_pubkey`, and
    /// would have to leave out `/block` and `/tx` — the two carrying the most
    /// data — to pass.
    #[test]
    fn no_endpoint_serves_the_wallets_private_key() {
        let (node, block) = a_mined_node();
        let hash = block.header().unwrap().hash();
        let (address, material) = {
            let held = node.lock().unwrap();
            (
                held.wallet.address().to_string(),
                held.wallet.key().material(),
            )
        };
        let mut reversed = material;
        reversed.reverse();

        for path in [
            "/status",
            "/mempool",
            "/peers",
            "/log",
            "/blocks",
            &format!("/address/{address}"),
            &format!("/block/{hash}"),
            &format!("/tx/{}", block.transactions[0].get_tx_id()),
        ] {
            let rendered = route(&get(path), &node).1.to_string();

            assert!(!rendered.contains(&hex::encode(material)), "{path}");
            assert!(!rendered.contains(&hex::encode(reversed)), "{path}");
        }
    }

    /// A header preceded by whitespace is obsolete line folding, not a
    /// header. Only the status line is parsed by most of these tests, so a
    /// response that reads fine to them can still be one a browser refuses.
    #[rstest]
    #[case::root("/", "text/html")]
    #[case::index("/index.html", "text/html")]
    #[case::style("/viewer.css", "text/css")]
    #[case::script("/viewer.js", "text/javascript")]
    fn the_viewer_is_served_by_the_same_server(#[case] path: &str, #[case] kind: &str) {
        let (address, _node) = a_served_node();
        let mut client = std::net::TcpStream::connect(address).unwrap();
        client
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .unwrap();

        let answer = read_all(&mut client);

        assert!(answer.starts_with("HTTP/1.1 200"), "{path}: {answer}");
        assert!(answer.contains(kind), "{path}: {answer}");
    }

    /// A page that fetched from a CDN is a page that breaks when the CDN does,
    /// and a deployment that is no longer one artefact.
    #[test]
    fn the_viewer_asks_nothing_of_anywhere_else() {
        for (_, body) in ["/", "/viewer.css", "/viewer.js"].map(|path| asset(path).unwrap()) {
            for elsewhere in ["http://", "https://", "//cdn", "integrity="] {
                assert!(!body.contains(elsewhere), "{elsewhere} in an asset");
            }
        }
    }

    /// The page reads what the API encoded; it must not be doing the encoding
    /// itself, which invariant 5 puts at the API's edge and nowhere else.
    ///
    /// A tripwire, not a proof — `/ 1e8` and a backwards `for` loop would both
    /// pass. It catches the spellings anybody would actually write, and it
    /// costs the page one idiom: the block list sorts by height rather than
    /// reversing the array, so a `reverse(` in the file is a finding rather
    /// than a false positive.
    #[test]
    fn the_viewer_does_not_re_encode_what_the_api_gave_it() {
        let script = asset("/viewer.js").unwrap().1;

        assert!(!script.contains("reverse("), "a hash reversed in the page");
        let atoms = ATOMS_PER_AVI.to_string();
        for spelling in [atoms.as_str(), "1e8", "10 ** 8", "Math.pow"] {
            assert!(
                !script.contains(spelling),
                "atoms divided into AVI in the page: {spelling}"
            );
        }
    }

    /// Every endpoint the page reaches, polled or clicked, answers. The
    /// string check alone would pass on a mention in a comment; the pair is
    /// what ties a name the page uses to a route that exists.
    #[test]
    fn every_endpoint_the_viewer_reaches_exists() {
        let (node, block) = a_mined_node();
        let hash = block.header().unwrap().hash();
        let txid = block.transactions[0].get_tx_id();
        let address = node.lock().unwrap().wallet.address().to_string();
        let script = asset("/viewer.js").unwrap().1;

        for polled in ["/status", "/mempool", "/peers", "/log"] {
            assert!(script.contains(&format!("\"{polled}\"")), "{polled}");
            assert_eq!(route(&get(polled), &node).0, 200, "{polled}");
        }

        for (used, path) in [
            ("/blocks?from=", "/blocks?from=0&count=12".to_string()),
            ("/block/${", format!("/block/{hash}")),
            ("/tx/${", format!("/tx/{txid}")),
            ("/address/${", format!("/address/{address}")),
        ] {
            assert!(script.contains(used), "{used} is not one the page builds");
            assert_eq!(route(&get(&path), &node).0, 200, "{path}");
        }

        assert!(script.contains("\"/tx\""), "the submit form posts to /tx");
        assert_eq!(
            route(&post("/tx", "deadbeef"), &node).0,
            400,
            "and it answers"
        );
    }

    /// A cross-origin page must not be able to make this node dial an address
    /// or hold a transaction. The side effect *is* the attack — the attacker
    /// never needs to read the response — and a `POST` with a simple body
    /// gets there with no preflight to refuse.
    #[test]
    fn a_write_from_somebody_elses_page_is_refused() {
        let node = a_node();
        let mut elsewhere = post("/connect", "127.0.0.1:5999");
        elsewhere.origin = Some("https://evil.example".to_string());
        elsewhere.host = Some("127.0.0.1:8080".to_string());

        let (status, body) = route(&elsewhere, &node);

        assert_eq!(status, 403, "{body}");
        assert_eq!(node.lock().unwrap().dialling, 0, "and nothing was dialled");
    }

    #[test]
    fn a_write_from_this_nodes_own_page_is_allowed() {
        let node = a_node();
        let mut ours = post("/connect", "127.0.0.1:5999");
        ours.origin = Some("http://127.0.0.1:8080".to_string());
        ours.host = Some("127.0.0.1:8080".to_string());

        assert_eq!(route(&ours, &node).0, 200);
    }

    #[test]
    fn a_head_of_the_page_carries_no_body() {
        let (address, _node) = a_served_node();
        let mut client = std::net::TcpStream::connect(address).unwrap();
        client
            .write_all(b"HEAD / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();

        let answer = read_all(&mut client);
        let (head, body) = answer.split_once("\r\n\r\n").unwrap();

        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body, "", "a HEAD answer has no body");
    }

    #[test]
    fn a_post_to_the_viewer_is_a_405_rather_than_a_page() {
        let (address, _node) = a_served_node();
        let mut client = std::net::TcpStream::connect(address).unwrap();
        client
            .write_all(
                b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();

        let answer = read_all(&mut client);

        assert!(answer.starts_with("HTTP/1.1 405"), "{answer}");
    }

    #[test]
    fn a_response_head_is_a_status_line_and_headers_with_nothing_in_front() {
        let (address, _node) = a_served_node();
        let mut client = std::net::TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();

        let answer = read_all(&mut client);
        let head = answer.split("\r\n\r\n").next().unwrap().to_string();

        for line in head.lines().skip(1) {
            assert_eq!(line, line.trim_start(), "a folded header: {line:?}");
            assert!(line.contains(": "), "not a header: {line:?}");
        }
        assert!(
            head.contains("\r\nContent-Type: application/json"),
            "{head:?}"
        );
        assert!(head.contains("\r\nContent-Length: "), "{head:?}");
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
