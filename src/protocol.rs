use crate::block::Header;
use crate::block::{Block, BlockHash};
use crate::blockchain::Accepted;
use crate::messages::addr::Addr;
use crate::messages::block::BlockMessage;
use crate::messages::getaddr::Getaddr;
use crate::messages::headers::{GetHeaders, Headers, MAX_HEADERS};
use crate::messages::inventory::{Inventory, Item, MAX_SERVED};
use crate::messages::message::MessageReceived::{
    AddrMessage, BlockMessageReceived, GetHeadersMessage, GetaddrMessage, GetdataMessage,
    HeadersMessage, InvMessage, PingMessage, PongMessage, TxMessage, VerackMessage, VersionMessage,
};
use crate::messages::message::{Message, MessageReceived};
use crate::messages::ping::Ping;
use crate::messages::pong::Pong;
use crate::messages::tx::Tx;
use crate::messages::verack::Verack;
use crate::messages::version::Version;
use crate::node::{
    record, Delivered, Handshake, HandshakeEvent, Identity, Node, Origin, PeerId, Refused,
    SharedNode, OUTBOUND_QUEUE,
};
use crate::params::Network;
use crate::transaction::{Transaction, Txid};
use crate::util::now;
use crate::validation::{check_spend, ClockDrift};
use anyhow::{anyhow, Result};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const PING_INTERVAL: Duration = Duration::from_secs(11);

/// A peer that has not accepted a byte in this long is not slow, it is gone.
/// Without it `write_all` blocks forever on a socket whose peer stopped
/// reading, and no amount of dropping the peer elsewhere can end that.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a connection may go without identifying itself. It doubles as the
/// read half's timeout, so a silent peer wakes the reader rather than parking it
/// against a deadline it cannot see.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// How many discovered addresses may be part-way through `connect` at once, and
/// how long each may take. ADR-0017: the bound is its own budget rather than the
/// peer table, or unroutable gossip denies the node every slot it has.
const MAX_DIALS_IN_FLIGHT: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// ADR-0016.
#[derive(Clone, Copy, Debug)]
pub struct Retry {
    pub first: Duration,
    pub cap: Duration,
    pub settled: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Retry {
            first: Duration::from_secs(1),
            cap: Duration::from_secs(60),
            settled: Duration::from_secs(10),
        }
    }
}

pub fn keep_connected(address: SocketAddr, node: SharedNode, retry: Retry) {
    let mut backoff = Backoff::new(retry.first, retry.cap);

    loop {
        thread::sleep(backoff.after(dial(address, &node), retry.settled));
    }
}

/// Dials, serves the connection to completion, and reports how long it lasted.
/// Waiting here is what keeps a live connection from being dialled twice.
fn dial(address: SocketAddr, node: &SharedNode) -> Duration {
    let stream = match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(e) => {
            record(node, format!("Could not connect to {address}: {e:#}"));
            // A dial that never connected lasted no time at all, however long
            // it spent failing.
            return Duration::ZERO;
        }
    };

    let opened = Instant::now();
    let serving = Arc::clone(node);

    // Joined, so a panic costs one connection rather than every future dial
    // from the loop that called us.
    let served = thread::spawn(move || serve_connection(stream, &serving, Origin::Dialled));

    if served.join().is_err() {
        record(node, format!("Connection with {address} panicked"));
    }

    opened.elapsed()
}

fn dial_if_wanted(address: SocketAddr, node: &SharedNode) {
    {
        let held = node.lock().expect("node lock poisoned");

        if address == held.config.host_address
            || held.peers.knows(address)
            || !held.peers.has_room()
        {
            return;
        }
    }

    let Some(budget) = Dialling::start(node) else {
        return;
    };

    let node = Arc::clone(node);
    thread::spawn(move || {
        let _budget = budget;
        dial(address, &node);
    });
}

/// One discovery dial's share of the budget, given back when it finishes.
struct Dialling(SharedNode);

impl Dialling {
    fn start(node: &SharedNode) -> Option<Dialling> {
        let mut held = node.lock().expect("node lock poisoned");

        if held.dialling >= MAX_DIALS_IN_FLIGHT {
            return None;
        }

        held.dialling += 1;
        Some(Dialling(Arc::clone(node)))
    }
}

impl Drop for Dialling {
    fn drop(&mut self) {
        let mut node = self.0.lock().unwrap_or_else(|held| held.into_inner());
        node.dialling -= 1;
    }
}

struct Backoff {
    next: Duration,
    first: Duration,
    cap: Duration,
}

impl Backoff {
    fn new(first: Duration, cap: Duration) -> Self {
        Backoff {
            next: first,
            first,
            cap,
        }
    }

    // ADR-0016.
    fn after(&mut self, lasted: Duration, settled: Duration) -> Duration {
        if lasted >= settled {
            self.next = self.first;
        }

        let waiting = self.next;
        self.next = (self.next * 2).min(self.cap);

        waiting
    }
}

pub fn listen(listener: TcpListener, node: SharedNode) -> Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => spawn_connection(stream, Arc::clone(&node), Origin::Accepted),
            Err(e) => record(&node, format!("Could not accept a connection: {e}")),
        }
    }

    Ok(())
}

fn spawn_connection(stream: TcpStream, node: SharedNode, origin: Origin) {
    thread::spawn(move || serve_connection(stream, &node, origin));
}

// Registration lives here, not in the call sites that listen and dial.
fn serve_connection(stream: TcpStream, node: &SharedNode, origin: Origin) {
    let peer = match stream.peer_addr() {
        Ok(peer) => peer,
        Err(e) => {
            record(
                node,
                format!("Dropping a connection with no resolvable peer address: {e}"),
            );
            return;
        }
    };

    let (outbound, queued) = mpsc::sync_channel(OUTBOUND_QUEUE);

    let queued_bytes = Arc::new(AtomicUsize::new(0));
    let registered = match Registered::open(node, peer, origin, outbound, Arc::clone(&queued_bytes))
    {
        Ok(registered) => registered,
        Err(refusal) => {
            record(
                node,
                format!("Refusing a connection with {peer}: {refusal:?}"),
            );
            return;
        }
    };

    if let Err(e) = handle_connection(stream, registered, queued, queued_bytes, HANDSHAKE_TIMEOUT) {
        record(node, format!("Connection with {peer} ended: {e:#}"));
    }
}

struct Registered {
    node: SharedNode,
    id: PeerId,
    address: SocketAddr,
    network: Network,
}

impl Registered {
    fn open(
        node: &SharedNode,
        peer: SocketAddr,
        origin: Origin,
        outbound: SyncSender<Vec<u8>>,
        queued_bytes: Arc<AtomicUsize>,
    ) -> Result<Registered, Refused> {
        let (id, network) = {
            let mut held = node.lock().expect("node lock poisoned");
            let network = held.config.network;
            (
                held.peers.register(peer, origin, outbound, queued_bytes)?,
                network,
            )
        };

        Ok(Registered {
            node: Arc::clone(node),
            id,
            address: peer,
            network,
        })
    }

    fn record(&self, entry: impl Into<String>) {
        record(&self.node, entry);
    }

    fn deliver(&self, message: Vec<u8>) -> Result<()> {
        let reached = self
            .node
            .lock()
            .expect("node lock poisoned")
            .peers
            .send_to(self.id, message);

        match reached {
            // Declining to answer an unidentified peer is the gate working.
            Delivered::Yes | Delivered::NotReady => Ok(()),
            Delivered::Gone => Err(anyhow!("peer is gone, or too far behind to answer")),
        }
    }

    fn answer_handshake(&self, message: Vec<u8>) -> Result<()> {
        match self
            .node
            .lock()
            .expect("node lock poisoned")
            .peers
            .answer_handshake(self.id, message)
        {
            Delivered::Yes => Ok(()),
            other => Err(anyhow!("could not answer the handshake: {other:?}")),
        }
    }

    fn advance_handshake(&self, event: HandshakeEvent) -> Result<Handshake> {
        self.node
            .lock()
            .expect("node lock poisoned")
            .peers
            .advance_handshake(self.id, event)
    }

    fn identify(&self, nonce: u64, listening: SocketAddr) -> Identity {
        self.node
            .lock()
            .expect("node lock poisoned")
            .identify(self.id, nonce, listening)
    }

    fn is_ready(&self) -> bool {
        is_ready(&self.node, self.id)
    }

    /// Into the mempool, or not at all — in three steps, with the lock held
    /// for the first and the last (ADR-0020).
    ///
    /// The middle one is a signature check and a script per input, and it runs
    /// with nothing held. Between the two halves a block can connect and spend
    /// the very coins this was checked against, so `admit` confirms every one
    /// of them is still there and unchanged before holding anything.
    fn accept(&self, transaction: Transaction) -> Result<Txid> {
        let txid = transaction.get_tx_id();

        let (coins, spend_height, network) = {
            let held = self.node.lock().expect("node lock poisoned");
            held.mempool.admissible(txid, &transaction)?;

            let network = held.config.network;
            (
                held.utxo.coins_for(&transaction),
                held.chain.height() + 1,
                network,
            )
        };

        let fee = check_spend(&transaction, &coins, spend_height, network)?;

        let mut held = self.node.lock().expect("node lock poisoned");
        // Read again, not reused: a reorg can lower the tip while a signature
        // is being checked, and maturity is measured against where the chain
        // is now.
        let spend_height = held.chain.height() + 1;
        let Node { mempool, utxo, .. } = &mut *held;

        mempool.admit(transaction, &coins, fee, utxo, spend_height, network)
    }

    /// Everyone Ready but the peer it came from, who already has it.
    fn offer(&self, item: Item) -> Result<()> {
        let offer = Message::new(Inventory::offered(vec![item]), self.network)?.get_raw_format()?;

        self.node
            .lock()
            .expect("node lock poisoned")
            .peers
            .relay(&offer, Some(self.id));

        Ok(())
    }

    /// How many bodies to have in flight at once. Small, because each is a
    /// megabyte and a peer that never sends them should cost us little.
    const IN_FLIGHT: usize = MAX_SERVED;

    /// How many headers to check under one lock. Small enough that a peer's
    /// batch cannot stall the node, large enough not to thrash.
    const HEADER_BATCH: usize = 64;

    /// Records what checks out and reports how many were new. A header that
    /// does not is not a reason to hang up: a peer on another branch will
    /// offer headers we cannot connect, and that is ordinary.
    /// Returns how many were new, and how many blocks the best chain is now
    /// missing a body for.
    fn take_headers(&self, headers: Vec<Header>) -> (usize, usize) {
        let mut taken = 0;

        // In chunks, so a batch of two thousand does not hold the node still
        // for two thousand ancestor walks. Each header costs a retarget window
        // and a median span to check.
        for batch in headers.chunks(Self::HEADER_BATCH) {
            let mut held = self.node.lock().expect("node lock poisoned");
            let network = held.config.network;
            let now = now();

            taken += batch
                .iter()
                .filter(|header| held.chain.add_header(**header, now, network).is_ok())
                .count();
        }

        let missing = self
            .node
            .lock()
            .expect("node lock poisoned")
            .chain
            .bodies_missing();

        (taken, missing)
    }

    fn bodies_wanted(&self) -> Vec<BlockHash> {
        self.node
            .lock()
            .expect("node lock poisoned")
            .chain
            .bodies_wanted(Self::IN_FLIGHT)
    }

    fn ask_for_headers(&self) -> Result<()> {
        let locator = self
            .node
            .lock()
            .expect("node lock poisoned")
            .chain
            .locator();

        self.deliver(Message::new(GetHeaders::new(locator), self.network)?.get_raw_format()?)
    }

    fn holds_block(&self, hash: &BlockHash) -> bool {
        self.node
            .lock()
            .expect("node lock poisoned")
            .chain
            .holds(hash)
    }

    fn take_block(&self, block: Block) -> Result<Accepted> {
        let mut held = self.node.lock().expect("node lock poisoned");
        let network = held.config.network;
        let Node {
            chain,
            utxo,
            mempool,
            ..
        } = &mut *held;

        chain.accept(block, utxo, mempool, now(), network)
    }

    // ADR-0017.
    fn announce(&self) -> Result<()> {
        let listening = self
            .node
            .lock()
            .expect("node lock poisoned")
            .peers
            .listening_of(self.id);

        let Some(listening) = listening else {
            return Ok(());
        };

        let news = Message::new(Addr::new(vec![listening]), self.network)?.get_raw_format()?;
        self.node
            .lock()
            .expect("node lock poisoned")
            .peers
            .relay(&news, Some(self.id));

        Ok(())
    }

    /// The writer thread cannot hold the registration — the table's sender has
    /// to be the only one — so it gets this instead.
    fn readiness(&self) -> impl Fn() -> bool {
        let node = Arc::clone(&self.node);
        let id = self.id;

        move || is_ready(&node, id)
    }
}

fn is_ready(node: &SharedNode, id: PeerId) -> bool {
    node.lock()
        .expect("node lock poisoned")
        .peers
        .handshake_of(id)
        .is_some_and(Handshake::is_ready)
}

impl Drop for Registered {
    fn drop(&mut self) {
        // Recovering the guard rather than unwrapping: this runs while a panic
        // may already be unwinding, and panicking again would abort.
        let mut node = self.node.lock().unwrap_or_else(|held| held.into_inner());
        node.peers.remove(self.id);
    }
}

struct ShutdownOnDrop(TcpStream);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        // The reader's own timeout is the handshake's, so on an established
        // connection a shutdown is the only thing that wakes it promptly.
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

fn handle_connection(
    stream: TcpStream,
    registered: Registered,
    queued: Receiver<Vec<u8>>,
    queued_bytes: Arc<AtomicUsize>,
    handshake_timeout: Duration,
) -> Result<()> {
    let write_half = ShutdownOnDrop(stream.try_clone()?);
    write_half.0.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.set_read_timeout(Some(handshake_timeout))?;

    let (host_address, peers, nonce) = {
        let node = registered.node.lock().expect("node lock poisoned");
        (node.config.host_address, node.peers.len(), node.nonce)
    };
    let network = registered.network;
    registered.record(format!(
        "{host_address} is handling a connection from {} ({peers} peers)",
        registered.address
    ));

    let ours = Message::new(Version::new(nonce, host_address), network)?.get_raw_format()?;
    let ready = registered.readiness();
    let writer = thread::spawn(move || {
        write_loop(
            &write_half.0,
            queued,
            PING_INTERVAL,
            ours,
            ready,
            network,
            queued_bytes,
        )
    });

    let read_result = read_loop(stream, &registered, handshake_timeout);

    // Before the join, not after: the table holds this peer's only sender, and
    // while it does the writer never sees the disconnect that ends it.
    drop(registered);

    match writer.join() {
        Ok(write_result) => read_result.and(write_result),
        Err(_) => Err(anyhow!("writer thread panicked")),
    }
}

fn write_loop<W: Write>(
    mut writer: W,
    queued: Receiver<Vec<u8>>,
    ping_interval: Duration,
    opening: Vec<u8>,
    ready: impl Fn() -> bool,
    network: Network,
    queued_bytes: Arc<AtomicUsize>,
) -> Result<()> {
    // Ahead of the queue, not in it, so nothing we enqueue can precede it.
    writer.write_all(&opening)?;

    let mut next_ping = Instant::now() + ping_interval;

    loop {
        if Instant::now() >= next_ping {
            if ready() {
                writer.write_all(&Message::new(Ping::new(), network)?.get_raw_format()?)?;
            }
            next_ping = Instant::now() + ping_interval;
        }

        match queued.recv_timeout(next_ping.saturating_duration_since(Instant::now())) {
            Ok(bytes) => {
                // Subtracted before the write, not after: the bytes have left
                // the queue, and a slow socket must not read as a full one.
                queued_bytes.fetch_sub(bytes.len(), Ordering::Relaxed);
                writer.write_all(&bytes)?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn read_loop<R: Read>(
    mut reader: R,
    registered: &Registered,
    handshake_timeout: Duration,
) -> Result<()> {
    let mut buffer = [0u8; 4096];
    let mut recv_buffer: Vec<u8> = Vec::new();
    let handshake_by = Instant::now() + handshake_timeout;
    let mut ready = false;

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                registered.record(format!("Connection with {} closed", registered.address));
                return Ok(());
            }
            Ok(n) => process_incoming_bytes(registered, &mut recv_buffer, &buffer[..n])?,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if expired(&e) => {}
            Err(e) => return Err(e.into()),
        }

        // Absolute, not per-read: a peer dribbling legal traffic would reset a
        // per-read deadline forever. The latch keeps a settled peer off the lock.
        if !ready {
            ready = registered.is_ready();

            if !ready && Instant::now() >= handshake_by {
                return Err(anyhow!(
                    "no handshake from {} within {handshake_timeout:?}",
                    registered.address
                ));
            }
        }
    }
}

fn expired(e: &std::io::Error) -> bool {
    // A read timeout is WouldBlock on Unix and TimedOut on Windows.
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn process_incoming_bytes(
    registered: &Registered,
    recv_buffer: &mut Vec<u8>,
    buffer: &[u8],
) -> Result<()> {
    recv_buffer.extend(buffer);
    while let (Some(message), bytes_consumed) =
        MessageReceived::try_parse_message(recv_buffer, registered.network)?
    {
        recv_buffer.drain(0..bytes_consumed);

        handle_messages(registered, message)?
    }
    Ok(())
}

fn handle_messages(registered: &Registered, message: MessageReceived) -> Result<()> {
    match message {
        VersionMessage(version) => {
            let peer = version.payload;
            registered.advance_handshake(HandshakeEvent::Version)?;

            match registered.identify(peer.nonce, peer.listen_address) {
                Identity::Ourselves => {
                    registered.record(format!("{} is us; hanging up", registered.address));
                    return Err(anyhow!("dialled ourselves"));
                }
                Identity::AlreadyConnected => {
                    registered.record(format!(
                        "{} is a peer we already hold; keeping the other connection",
                        registered.address
                    ));
                    return Err(anyhow!("already connected to this peer"));
                }
                Identity::New => {}
            }

            registered.record(format!(
                "{} speaks protocol {} and listens on {}",
                registered.address, peer.protocol_version, peer.listen_address
            ));
            registered
                .answer_handshake(Message::new(Verack, registered.network)?.get_raw_format()?)?;
        }
        VerackMessage => {
            registered.advance_handshake(HandshakeEvent::Verack)?;
            registered.record(format!("Handshake with {} complete", registered.address));

            // Nothing else wakes the writer, whose timer is an interval away.
            registered.deliver(Message::new(Ping::new(), registered.network)?.get_raw_format()?)?;
            registered.deliver(Message::new(Getaddr, registered.network)?.get_raw_format()?)?;
            registered.announce()?;
            // Whether we are behind is the peer's answer to give, and a
            // locator is how it is asked without either of us sending a chain.
            registered.ask_for_headers()?;
        }
        // One gate for everything that is not the handshake itself, so a
        // message type added later cannot quietly skip it. A stranger's `addr`
        // would otherwise have us dialling on its say-so.
        _ if !registered.is_ready() => {}
        GetaddrMessage => {
            let known = registered
                .node
                .lock()
                .expect("node lock poisoned")
                .peers
                .listening_addresses(registered.id);

            registered
                .deliver(Message::new(Addr::new(known), registered.network)?.get_raw_format()?)?;
        }
        AddrMessage(addr) => {
            registered.record(format!(
                "{} offered {} addresses",
                registered.address,
                addr.payload.addresses.len()
            ));

            for address in addr.payload.addresses {
                dial_if_wanted(address, &registered.node);
            }
        }
        PingMessage(ping) => {
            registered.record(format!("Ping received {ping:?}"));
            let pong = Pong::new(ping.payload)?;
            registered.deliver(Message::new(pong, registered.network)?.get_raw_format()?)?;
        }
        PongMessage(pong) => registered.record(format!("Pong received {pong:?}")),
        InvMessage(inv) => {
            let wanted: Vec<Item> = {
                let node = registered.node.lock().expect("node lock poisoned");
                inv.payload
                    .items
                    .into_iter()
                    .filter(|item| match item {
                        Item::Transaction(txid) => !node.mempool.contains(txid),
                        Item::Block(hash) => !node.chain.holds(hash),
                    })
                    .collect()
            };

            // To this peer only. A broadcast here would have every peer
            // fetching what one of them offered.
            if !wanted.is_empty() {
                registered.deliver(
                    Message::new(Inventory::requested(wanted), registered.network)?
                        .get_raw_format()?,
                )?;
            }
        }
        GetdataMessage(getdata) => {
            // Gathered under one lock, sent outside it — the same shape as the
            // `inv` arm above, and the reason `record` prints before locking.
            let (transactions, blocks) = {
                let node = registered.node.lock().expect("node lock poisoned");
                let mut transactions = Vec::new();
                let mut blocks = Vec::new();

                for item in getdata.payload.items {
                    match item {
                        Item::Transaction(txid) => {
                            transactions.extend(node.mempool.get(&txid).cloned())
                        }
                        Item::Block(hash) => blocks.extend(node.chain.body(&hash).cloned()),
                    }
                }

                (transactions, blocks)
            };

            for transaction in transactions {
                registered.deliver(
                    Message::new(Tx::new(transaction), registered.network)?.get_raw_format()?,
                )?;
            }
            for block in blocks {
                registered.deliver(
                    Message::new(BlockMessage::new(block), registered.network)?.get_raw_format()?,
                )?;
            }
        }
        GetHeadersMessage(request) => {
            let headers = {
                let node = registered.node.lock().expect("node lock poisoned");
                node.chain
                    .headers_after(&request.payload.locator, &request.payload.stop)
            };

            if !headers.is_empty() {
                registered.deliver(
                    Message::new(Headers::new(headers), registered.network)?.get_raw_format()?,
                )?;
            }
        }
        HeadersMessage(message) => {
            let offered = message.payload.headers.len();
            let (taken, behind) = registered.take_headers(message.payload.headers);
            if taken > 0 {
                // How far behind we are, which is what an operator wants from
                // a sync and what a start-height field in `version` would only
                // have guessed at.
                registered.record(format!(
                    "{} offered {offered} headers, {taken} new; {behind} blocks to fetch",
                    registered.address
                ));
            }

            // Bodies only once the headers have shown their work, and only if
            // any of them were new — a peer repeating itself should cost a
            // lookup, not a walk of the chain.
            let wanted: Vec<Item> = if taken == 0 {
                Vec::new()
            } else {
                registered
                    .bodies_wanted()
                    .into_iter()
                    .map(Item::Block)
                    .collect()
            };
            if !wanted.is_empty() {
                registered.deliver(
                    Message::new(Inventory::requested(wanted), registered.network)?
                        .get_raw_format()?,
                )?;
            }

            // More where those came from: a full batch we could use means the
            // peer has more. Asking again on a batch that taught us nothing is
            // how two nodes talk forever.
            if offered == MAX_HEADERS && taken > 0 {
                registered.ask_for_headers()?;
            }
        }
        BlockMessageReceived(message) => {
            let block = message.payload.block.clone();
            let hash = block.header()?.hash();

            let parent = message.payload.block.header()?.previous_block_hash;

            match registered.take_block(block) {
                Ok(Accepted::Orphaned(_)) => {
                    // It arrived before its parent. Ask this peer for that,
                    // and the walk repeats until one of them connects — but
                    // never for something already waiting here, which is what
                    // stops two orphans naming each other bouncing forever.
                    // Bounded by the orphan pool: a block is only held once it
                    // has been shown to meet its own target. #91 replaces the
                    // walk with headers-first sync.
                    if !registered.holds_block(&parent) {
                        registered.deliver(
                            Message::new(
                                Inventory::requested(vec![Item::Block(parent)]),
                                registered.network,
                            )?
                            .get_raw_format()?,
                        )?;
                    }
                }
                Ok(Accepted::Held(_)) => {}
                Ok(outcome) => {
                    registered.record(format!("{} sent {hash}: {outcome:?}", registered.address));
                    registered.offer(Item::Block(hash))?;
                }
                Err(refusal) => registered.record(match refusal.downcast_ref::<ClockDrift>() {
                    // ADR-0009: a node whose own clock is wrong rejects what
                    // everyone else accepts, and that reads as a partition.
                    Some(drift) => format!("REFUSING BLOCKS: {drift}"),
                    None => format!(
                        "{} sent a block we will not take: {refusal:#}",
                        registered.address
                    ),
                }),
            }
        }
        TxMessage(tx) => {
            // Validated exactly as strictly as one we asked for: relay is not
            // a way around validation.
            match registered.accept(tx.payload.transaction) {
                Ok(txid) => {
                    registered.record(format!("{} relayed {txid}", registered.address));
                    registered.offer(Item::Transaction(txid))?;
                }
                Err(why) => registered.record(format!(
                    "{} sent a transaction we will not hold: {why:#}",
                    registered.address
                )),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::Amount;
    use crate::config::Config;
    use crate::node::Node;
    use crate::params::MAINNET;
    use rstest::rstest;

    const NEVER: Duration = Duration::from_secs(3600);

    fn framed<P: crate::messages::message::Payload>(payload: P) -> Vec<u8> {
        framed_on(payload, &MAINNET)
    }

    /// The same, on whichever network the peer is speaking — a test that
    /// builds mainnet frames for a testnet node is testing the magic filter.
    fn framed_on<P: crate::messages::message::Payload>(payload: P, network: Network) -> Vec<u8> {
        Message::new(payload, network)
            .unwrap()
            .get_raw_format()
            .unwrap()
    }

    fn framed_ping() -> (Vec<u8>, u64) {
        let ping = Ping::new();
        let nonce = ping.nonce;
        (framed(ping), nonce)
    }

    fn framed_version() -> Vec<u8> {
        framed_version_of(7, A_LISTEN_ADDRESS)
    }

    fn framed_version_of(nonce: u64, listening: &str) -> Vec<u8> {
        framed(Version::new(nonce, listening.parse().unwrap()))
    }

    const A_LISTEN_ADDRESS: &str = "127.0.0.1:5000";

    /// What a peer sends to be counted: its version, then a verack for ours.
    fn identify(registered: &Registered) {
        identify_as(registered, 7, A_LISTEN_ADDRESS);
    }

    fn identify_as(registered: &Registered, nonce: u64, listening: &str) {
        let network = registered.network;
        let both = [
            framed_on(Version::new(nonce, listening.parse().unwrap()), network),
            framed_on(Verack, network),
        ]
        .concat();

        process_incoming_bytes(registered, &mut Vec::new(), &both)
            .expect("a well-formed handshake should be accepted");
    }

    fn parse_all(bytes: &[u8]) -> Vec<MessageReceived> {
        parse_all_on(bytes, &MAINNET)
    }

    fn parse_all_on(bytes: &[u8], network: Network) -> Vec<MessageReceived> {
        let mut rest = bytes;
        let mut messages = Vec::new();

        while let (Some(message), consumed) =
            MessageReceived::try_parse_message(rest, network).unwrap()
        {
            messages.push(message);
            rest = &rest[consumed..];
        }

        assert!(rest.is_empty(), "{} trailing bytes", rest.len());
        messages
    }

    #[test]
    fn a_connection_opens_with_its_version_and_nothing_else() {
        let (outbound, queued) = mpsc::sync_channel(OUTBOUND_QUEUE);
        drop(outbound);

        let mut output = Vec::new();
        write_loop(
            &mut output,
            queued,
            Duration::ZERO,
            framed_version(),
            || false,
            &MAINNET,
            Arc::new(AtomicUsize::new(0)),
        )
        .unwrap();

        assert!(
            matches!(parse_all(&output).as_slice(), [VersionMessage(_)]),
            "a peer that has not identified itself is owed nothing but our version, \
             and a zero interval means the timer had every chance to fire"
        );
    }

    #[test]
    fn a_ready_peer_is_pinged() {
        let (outbound, queued) = mpsc::sync_channel(OUTBOUND_QUEUE);
        drop(outbound);

        let mut output = Vec::new();
        write_loop(
            &mut output,
            queued,
            Duration::ZERO,
            framed_version(),
            || true,
            &MAINNET,
            Arc::new(AtomicUsize::new(0)),
        )
        .unwrap();

        assert!(
            parse_all(&output)
                .iter()
                .any(|message| matches!(message, PingMessage(_))),
            "a Ready peer should be pinged"
        );
    }

    #[test]
    fn a_message_enqueued_from_another_thread_is_written_to_the_peer() {
        let (outbound, queued) = mpsc::sync_channel(OUTBOUND_QUEUE);
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
        write_loop(
            &mut output,
            queued,
            NEVER,
            Vec::new(),
            || true,
            &MAINNET,
            Arc::new(AtomicUsize::new(0)),
        )
        .unwrap();
        sender.join().unwrap();

        match parse_all(&output).as_slice() {
            [PongMessage(pong)] => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected the enqueued pong, got {other:?}"),
        }
    }

    #[test]
    fn a_stalled_write_ends_the_connection_rather_than_blocking_forever() {
        /// Takes the opening version, then behaves like a socket whose write
        /// timeout has expired — so the failure under test is the *queued*
        /// message, not the opening one.
        #[derive(Default)]
        struct AcceptsThenStalls {
            taken: usize,
        }

        impl Write for AcceptsThenStalls {
            fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
                if self.taken == 0 {
                    self.taken += 1;
                    return Ok(buffer.len());
                }
                Err(std::io::Error::from(ErrorKind::WouldBlock))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (outbound, queued) = mpsc::sync_channel(OUTBOUND_QUEUE);
        outbound.try_send(b"backlog".to_vec()).unwrap();
        drop(outbound);

        // Dropping the peer cannot end this connection on its own: mpsc hands
        // the writer every buffered message before it ever reports
        // Disconnected, so the writer must give up on the socket itself.
        write_loop(
            AcceptsThenStalls::default(),
            queued,
            NEVER,
            framed_version(),
            || true,
            &MAINNET,
            Arc::new(AtomicUsize::new(0)),
        )
        .expect_err("a write that cannot proceed must end the connection");
    }

    const SETTLED: Duration = Duration::from_millis(10);

    fn a_backoff() -> Backoff {
        Backoff::new(Duration::from_millis(1), Duration::from_millis(8))
    }

    #[test]
    fn a_dial_that_never_connected_lasted_no_time_at_all() {
        let refused = a_free_port();

        let lasted = dial(refused, &a_node());

        // Not the time the *attempt* took: a blackholed address can sit in
        // connect() for a minute, and treating that as a connection that lasted
        // would reset the backoff on exactly the peer it exists to back off.
        assert_eq!(Duration::ZERO, lasted);
    }

    #[test]
    fn a_dial_that_connected_reports_how_long_it_was_served() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let hangs_up = thread::spawn(move || drop(listener.accept().unwrap()));

        let lasted = dial(address, &a_node());
        hangs_up.join().unwrap();

        assert!(lasted > Duration::ZERO, "a connection that happened lasted");
    }

    #[test]
    fn a_backoff_grows_and_stops_at_its_cap() {
        let mut backoff = a_backoff();

        let waits: Vec<_> = (0..6)
            .map(|_| backoff.after(Duration::ZERO, SETTLED).as_millis())
            .collect();

        assert_eq!(
            vec![1, 2, 4, 8, 8, 8],
            waits,
            "a peer that is simply gone must not become a busy loop, nor an \
             ever-growing wait"
        );
    }

    #[test]
    fn a_connection_that_did_not_last_leaves_the_backoff_growing() {
        let mut backoff = a_backoff();

        let waits: Vec<_> = (0..4)
            .map(|_| {
                backoff
                    .after(SETTLED - Duration::from_millis(1), SETTLED)
                    .as_millis()
            })
            .collect();

        assert_eq!(
            vec![1, 2, 4, 8],
            waits,
            "connecting is not succeeding: a peer that hangs up at once — which \
             is us, under the checked-in config — must not hold us at 1ms"
        );
    }

    #[test]
    fn a_connection_that_lasted_starts_the_backoff_over() {
        let mut backoff = a_backoff();
        for _ in 0..3 {
            backoff.after(Duration::ZERO, SETTLED);
        }

        assert_eq!(
            Duration::from_millis(1),
            backoff.after(SETTLED, SETTLED),
            "a peer that worked and then went should be tried again promptly"
        );
    }

    #[test]
    fn a_connection_bounds_how_long_a_write_may_block() {
        let (_peer, accepted, peer_addr) = a_connected_pair();
        let observer = accepted.try_clone().unwrap();

        thread::spawn(move || handle_alone(accepted, peer_addr, a_node()));

        eventually(
            || observer.write_timeout().unwrap().is_some(),
            "the write half never had its blocking bounded",
        );
        assert_eq!(Some(WRITE_TIMEOUT), observer.write_timeout().unwrap());
    }

    #[test]
    fn a_connection_bounds_how_long_it_waits_to_be_told_who_it_is_talking_to() {
        let (_peer, accepted, peer_addr) = a_connected_pair();
        let observer = accepted.try_clone().unwrap();

        thread::spawn(move || handle_alone(accepted, peer_addr, a_node()));

        eventually(
            || observer.read_timeout().unwrap().is_some(),
            "the read half never had its waiting bounded",
        );
        assert_eq!(Some(HANDSHAKE_TIMEOUT), observer.read_timeout().unwrap());
    }

    #[test]
    fn a_peer_that_never_identifies_itself_gives_its_slot_back() {
        let (_peer, accepted, peer_addr) = a_connected_pair();
        let node = a_node();
        let watched = Arc::clone(&node);

        thread::spawn(move || {
            handle_alone_for(accepted, peer_addr, node, Duration::from_millis(50))
        });

        eventually(
            || watched.lock().unwrap().peers.len() == 1,
            "the connection never registered a peer",
        );
        // The slot and its 32 MiB recv_buffer are what the deadline is for;
        // ending the connection without freeing them would miss the point.
        eventually(
            || watched.lock().unwrap().peers.is_empty(),
            "a peer that never identified itself kept its slot",
        );
    }

    #[test]
    fn pings_recur_at_the_configured_interval() {
        let interval = Duration::from_millis(20);
        let run_for = Duration::from_millis(300);

        let (outbound, queued) = mpsc::sync_channel::<Vec<u8>>(OUTBOUND_QUEUE);
        let holder = thread::spawn(move || {
            thread::sleep(run_for);
            drop(outbound);
        });

        let mut output = Vec::new();
        write_loop(
            &mut output,
            queued,
            interval,
            Vec::new(),
            || true,
            &MAINNET,
            Arc::new(AtomicUsize::new(0)),
        )
        .unwrap();
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
    fn a_getaddr_is_answered_with_where_the_other_peers_listen() {
        let node = a_node();
        let (registered, queued) = a_registered_peer_of(&node, "127.0.0.1:5001");
        let (other, _theirs) = a_registered_peer_of(&node, "127.0.0.1:5002");
        identify_as(&other, 8, A_LISTEN_ADDRESS);
        identify_as(&registered, 9, "127.0.0.1:5003");
        while queued.try_recv().is_ok() {}

        process_incoming_bytes(&registered, &mut Vec::new(), &framed(Getaddr)).unwrap();

        let reply = queued.try_recv().expect("a getaddr must be answered");
        match parse_all(&reply).as_slice() {
            [AddrMessage(addr)] => assert_eq!(
                vec![A_LISTEN_ADDRESS.parse::<SocketAddr>().unwrap()],
                addr.payload.addresses,
                "the listening address from their version, not their source port"
            ),
            other => panic!("expected an addr, got {other:?}"),
        }
    }

    #[test]
    fn a_getaddr_from_a_peer_that_has_not_identified_itself_is_not_answered() {
        let (registered, queued) = a_registered_peer();

        process_incoming_bytes(&registered, &mut Vec::new(), &framed(Getaddr))
            .expect("declining is not a broken connection");

        assert!(queued.try_recv().is_err());
    }

    #[test]
    fn an_address_we_already_hold_is_not_dialled_again() {
        let node = a_node();
        let (registered, _queued) = a_registered_peer_of(&node, "127.0.0.1:5001");
        identify(&registered);
        let before = node.lock().unwrap().peers.len();

        dial_if_wanted(A_LISTEN_ADDRESS.parse().unwrap(), &node);

        assert_eq!(
            before,
            node.lock().unwrap().peers.len(),
            "a peer we have is not a peer to go and find"
        );
    }

    #[test]
    fn our_own_listening_address_is_not_dialled() {
        let node = a_node();
        let ours = node.lock().unwrap().config.host_address;
        let before = node.lock().unwrap().peers.len();

        dial_if_wanted(ours, &node);

        assert_eq!(before, node.lock().unwrap().peers.len());
    }

    #[test]
    fn only_so_many_dials_may_be_part_way_through_at_once() {
        let node = a_node();
        let held: Vec<_> = (0..MAX_DIALS_IN_FLIGHT)
            .map(|_| Dialling::start(&node).expect("within the budget"))
            .collect();

        assert!(
            Dialling::start(&node).is_none(),
            "the budget bounds work in flight; without it an addr of unroutable \
             addresses buys a thread parked in connect() per entry"
        );

        drop(held);
        assert!(
            Dialling::start(&node).is_some(),
            "a dial that finished gives its share back"
        );
    }

    #[test]
    fn an_address_arriving_past_the_dial_budget_is_dropped_rather_than_queued() {
        let node = a_node();
        let _held: Vec<_> = (0..MAX_DIALS_IN_FLIGHT)
            .map(|_| Dialling::start(&node).expect("within the budget"))
            .collect();

        dial_if_wanted(a_free_port(), &node);

        assert!(
            node.lock().unwrap().peers.is_empty(),
            "no budget, no dial — and no thread to find out with"
        );
    }

    #[test]
    fn a_dial_in_flight_does_not_hold_a_peer_slot() {
        let node = a_node();

        dial_if_wanted(a_free_port(), &node);

        // Reserving the peer slot first would bound dialling by MAX_PEERS, but
        // an addr of unroutable addresses would then deny the node every slot
        // it has — inbound connections included — for the connect timeout.
        eventually(
            || node.lock().unwrap().dialling == 0,
            "the dial never finished",
        );
        assert!(node.lock().unwrap().peers.is_empty());
    }

    #[test]
    fn an_addr_from_a_peer_that_has_not_identified_itself_is_not_dialled_from() {
        let (registered, _queued) = a_registered_peer();
        let unwanted = a_free_port();
        let framed_addr = framed(Addr::new(vec![unwanted]));

        process_incoming_bytes(&registered, &mut Vec::new(), &framed_addr)
            .expect("ignoring it is not a broken connection");

        assert_eq!(
            0,
            registered.node.lock().unwrap().dialling,
            "a stranger's addr must not have us dialling on its say-so"
        );
    }

    #[test]
    fn a_ping_from_a_peer_that_has_not_identified_itself_is_not_answered() {
        let (registered, queued) = a_registered_peer();

        process_incoming_bytes(&registered, &mut Vec::new(), &framed_ping().0)
            .expect("declining to answer is not a broken connection");

        assert!(
            queued.try_recv().is_err(),
            "we owe a peer that has not said who it is nothing at all"
        );
    }

    #[test]
    fn an_inbound_ping_is_answered_with_a_pong_on_the_outbound_channel() {
        let (registered, queued) = a_registered_peer();
        identify(&registered);
        while queued.try_recv().is_ok() {}

        let mut recv_buffer = Vec::new();
        let (ping, nonce) = framed_ping();

        process_incoming_bytes(&registered, &mut recv_buffer, &ping).unwrap();

        assert!(recv_buffer.is_empty(), "the ping should be fully consumed");

        let reply = queued.try_recv().expect("a ping must be answered");
        match parse_all(&reply).as_slice() {
            [PongMessage(pong)] => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected a pong, got {other:?}"),
        }
        assert!(queued.try_recv().is_err(), "one ping, one pong");
    }

    /// On the test network, where a coinbase matures in one block — a mainnet
    /// node at height zero cannot spend anything for another hundred.
    fn a_node_funding(wallet: &crate::wallet::Wallet) -> SharedNode {
        let node = a_testnet_node();
        {
            let mut held = node.lock().unwrap();
            crate::validation::fixtures::funded(&mut held.utxo, wallet.key(), 10_000, 0);
        }

        node
    }

    fn a_payment(node: &SharedNode, wallet: &crate::wallet::Wallet) -> Transaction {
        let held = node.lock().unwrap();
        let spend_height = held.chain.height() + 1;

        wallet
            .build(&held.utxo, spend_height, &crate::params::TESTNET)
            .pay(
                &crate::wallet::Wallet::new().address().to_string(),
                Amount::from_atoms(4_000).unwrap(),
            )
            .unwrap()
            .sign()
            .unwrap()
    }

    fn framed_inv(txid: Txid) -> Vec<u8> {
        framed_on(
            Inventory::offered(vec![Item::Transaction(txid)]),
            &crate::params::TESTNET,
        )
    }

    fn framed_getdata(txid: Txid) -> Vec<u8> {
        framed_on(
            Inventory::requested(vec![Item::Transaction(txid)]),
            &crate::params::TESTNET,
        )
    }

    fn deliver(registered: &Registered, bytes: &[u8]) {
        process_incoming_bytes(registered, &mut Vec::new(), bytes).unwrap();
    }

    fn drain(queued: &Receiver<Vec<u8>>) -> Vec<MessageReceived> {
        drain_on(queued, &MAINNET)
    }

    fn drain_on(queued: &Receiver<Vec<u8>>, network: Network) -> Vec<MessageReceived> {
        let mut messages = Vec::new();
        while let Ok(bytes) = queued.try_recv() {
            messages.extend(parse_all_on(&bytes, network));
        }

        messages
    }

    #[test]
    fn an_inv_for_a_transaction_we_do_not_hold_asks_that_peer_and_no_other() {
        let wallet = crate::wallet::Wallet::new();
        let node = a_node_funding(&wallet);
        let (asker, asked) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        let (bystander, ignored) = a_registered_peer_of(&node, "127.0.0.1:5001");
        identify_as(&asker, 7, A_LISTEN_ADDRESS);
        identify_as(&bystander, 8, "127.0.0.1:5001");
        drain_on(&asked, &crate::params::TESTNET);
        drain_on(&ignored, &crate::params::TESTNET);

        let txid = a_payment(&node, &wallet).get_tx_id();
        deliver(&asker, &framed_inv(txid));

        match drain_on(&asked, &crate::params::TESTNET).as_slice() {
            [GetdataMessage(getdata)] => {
                assert_eq!(getdata.payload.items, vec![Item::Transaction(txid)])
            }
            other => panic!("expected one getdata, got {other:?}"),
        }
        assert!(
            drain_on(&ignored, &crate::params::TESTNET).is_empty(),
            "only the peer that offered it"
        );
    }

    #[test]
    fn an_inv_for_a_transaction_we_already_hold_is_not_asked_for_again() {
        let wallet = crate::wallet::Wallet::new();
        let node = a_node_funding(&wallet);
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        drain_on(&queued, &crate::params::TESTNET);

        let payment = a_payment(&node, &wallet);
        let txid = registered.accept(payment).unwrap();

        deliver(&registered, &framed_inv(txid));

        assert!(
            drain_on(&queued, &crate::params::TESTNET).is_empty(),
            "we have it; asking is a loop"
        );
    }

    #[test]
    fn a_getdata_is_answered_only_for_what_we_hold() {
        let wallet = crate::wallet::Wallet::new();
        let node = a_node_funding(&wallet);
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        drain_on(&queued, &crate::params::TESTNET);

        let payment = a_payment(&node, &wallet);
        let txid = registered.accept(payment.clone()).unwrap();

        deliver(&registered, &framed_getdata(Txid::from_bytes([9; 32])));
        assert!(
            drain_on(&queued, &crate::params::TESTNET).is_empty(),
            "we do not have that one"
        );

        deliver(&registered, &framed_getdata(txid));
        match drain_on(&queued, &crate::params::TESTNET).as_slice() {
            [TxMessage(tx)] => assert_eq!(tx.payload.transaction, payment),
            other => panic!("expected the transaction, got {other:?}"),
        }
    }

    #[test]
    fn an_accepted_transaction_is_offered_to_every_other_ready_peer() {
        let wallet = crate::wallet::Wallet::new();
        let node = a_node_funding(&wallet);
        let (sender, sent_back) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        let (other, told) = a_registered_peer_of(&node, "127.0.0.1:5001");
        identify_as(&sender, 7, A_LISTEN_ADDRESS);
        identify_as(&other, 8, "127.0.0.1:5001");
        drain_on(&sent_back, &crate::params::TESTNET);
        drain_on(&told, &crate::params::TESTNET);

        let payment = a_payment(&node, &wallet);
        let txid = payment.get_tx_id();
        deliver(
            &sender,
            &framed_on(Tx::new(payment), &crate::params::TESTNET),
        );

        match drain_on(&told, &crate::params::TESTNET).as_slice() {
            [InvMessage(inv)] => assert_eq!(inv.payload.items, vec![Item::Transaction(txid)]),
            other => panic!("expected an inv, got {other:?}"),
        }
        assert!(
            drain_on(&sent_back, &crate::params::TESTNET).is_empty(),
            "the peer that sent it already has it"
        );
    }

    #[test]
    fn a_transaction_that_does_not_validate_is_neither_held_nor_announced() {
        let wallet = crate::wallet::Wallet::new();
        let node = a_node_funding(&wallet);
        let (sender, sent_back) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        let (other, told) = a_registered_peer_of(&node, "127.0.0.1:5001");
        identify_as(&sender, 7, A_LISTEN_ADDRESS);
        identify_as(&other, 8, "127.0.0.1:5001");
        drain_on(&sent_back, &crate::params::TESTNET);
        drain_on(&told, &crate::params::TESTNET);

        let mut forged = a_payment(&node, &wallet);
        forged.outputs[0].value = Amount::from_atoms(9_999_999).unwrap();

        deliver(
            &sender,
            &framed_on(Tx::new(forged), &crate::params::TESTNET),
        );

        assert!(
            drain_on(&told, &crate::params::TESTNET).is_empty(),
            "nothing to announce"
        );
        assert!(node.lock().unwrap().mempool.is_empty());
    }

    /// A node on the test network, whose difficulty a test can mine against.
    /// `a_node` is mainnet, where a single block costs millions of hashes.
    fn a_testnet_node() -> SharedNode {
        Node::shared(
            Config {
                mine: false,
                network: &crate::params::TESTNET,
                host_address: "127.0.0.1:34352".parse().unwrap(),
                addresses_to_connect: Vec::new(),
            },
            &crate::params::TESTNET.genesis().unwrap(),
        )
        .unwrap()
    }

    fn a_block_on(node: &SharedNode, seed: u64) -> Block {
        let (parent, height, bits) = {
            let held = node.lock().unwrap();
            let tip = held.chain.tip();
            let entry = held.chain.index().get(&tip).unwrap();
            (
                entry.header,
                entry.height + 1,
                held.chain
                    .index()
                    .required_bits_after(&tip, held.config.network)
                    .unwrap(),
            )
        };

        let coinbase = crate::transaction::Transaction::coinbase(
            height,
            seed,
            vec![crate::validation::fixtures::pay_to(
                &crate::crypto::PrivateKey::random(),
                crate::amount::subsidy(height).atoms(),
            )],
        );
        let mut block = Block::new(
            1,
            *parent.hash().as_bytes(),
            parent.time + 30,
            bits,
            vec![coinbase],
        );
        assert!(block.mine().unwrap());

        block
    }

    #[test]
    fn an_inv_for_a_block_we_do_not_hold_asks_for_it() {
        let node = a_testnet_node();
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        drain_on(&queued, registered.network);
        let hash = a_block_on(&node, 1).header().unwrap().hash();

        deliver(
            &registered,
            &framed_on(
                Inventory::offered(vec![Item::Block(hash)]),
                registered.network,
            ),
        );

        match drain_on(&queued, registered.network).as_slice() {
            [GetdataMessage(getdata)] => assert_eq!(getdata.payload.items, vec![Item::Block(hash)]),
            other => panic!("expected one getdata, got {other:?}"),
        }
    }

    #[test]
    fn an_inv_for_a_block_we_hold_produces_nothing() {
        let node = a_testnet_node();
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        let block = a_block_on(&node, 1);
        let hash = block.header().unwrap().hash();
        registered.take_block(block).unwrap();
        drain_on(&queued, registered.network);

        deliver(
            &registered,
            &framed_on(
                Inventory::offered(vec![Item::Block(hash)]),
                registered.network,
            ),
        );

        assert!(
            drain_on(&queued, registered.network).is_empty(),
            "we have it already"
        );
    }

    #[test]
    fn a_getdata_for_a_block_we_hold_sends_it() {
        let node = a_testnet_node();
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        let block = a_block_on(&node, 1);
        let hash = block.header().unwrap().hash();
        registered.take_block(block.clone()).unwrap();
        drain_on(&queued, registered.network);

        deliver(
            &registered,
            &framed_on(
                Inventory::requested(vec![Item::Block(hash)]),
                registered.network,
            ),
        );

        match drain_on(&queued, registered.network).as_slice() {
            [BlockMessageReceived(sent)] => assert_eq!(sent.payload.block, block),
            other => panic!("expected the block, got {other:?}"),
        }
    }

    #[test]
    fn a_getdata_for_a_block_we_do_not_hold_sends_nothing() {
        let node = a_testnet_node();
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        drain_on(&queued, registered.network);

        deliver(
            &registered,
            &framed_on(
                Inventory::requested(vec![Item::Block(crate::block::BlockHash::from_bytes(
                    [4; 32],
                ))]),
                registered.network,
            ),
        );

        assert!(drain_on(&queued, registered.network).is_empty());
    }

    #[test]
    fn a_block_that_extends_the_chain_is_offered_onward() {
        let node = a_testnet_node();
        let (sender, sent_back) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        let (other, told) = a_registered_peer_of(&node, "127.0.0.1:5001");
        identify_as(&sender, 7, A_LISTEN_ADDRESS);
        identify_as(&other, 8, "127.0.0.1:5001");
        drain_on(&sent_back, sender.network);
        drain_on(&told, other.network);

        let block = a_block_on(&node, 1);
        let hash = block.header().unwrap().hash();
        deliver(
            &sender,
            &framed_on(BlockMessage::new(block), sender.network),
        );

        match drain_on(&told, other.network).as_slice() {
            [InvMessage(inv)] => assert_eq!(inv.payload.items, vec![Item::Block(hash)]),
            got => panic!("expected an inv, got {got:?}"),
        }
        assert!(
            drain_on(&sent_back, sender.network).is_empty(),
            "the peer that sent it has it"
        );
        assert_eq!(node.lock().unwrap().chain.height(), 1);
    }

    #[test]
    fn headers_are_answered_from_where_a_locator_says_the_peer_is() {
        let node = a_testnet_node();
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        let mut at = node.lock().unwrap().chain.tip();
        for seed in 1..=4u64 {
            let block = a_block_on(&node, seed);
            at = block.header().unwrap().hash();
            registered.take_block(block).unwrap();
        }
        drain_on(&queued, registered.network);

        let locator = node.lock().unwrap().chain.locator();
        deliver(
            &registered,
            &framed_on(GetHeaders::new(locator), registered.network),
        );

        assert!(
            drain_on(&queued, registered.network).is_empty(),
            "a peer already at our tip is told nothing"
        );

        let genesis = *node
            .lock()
            .unwrap()
            .chain
            .index()
            .best_chain()
            .first()
            .unwrap();
        deliver(
            &registered,
            &framed_on(GetHeaders::new(vec![genesis]), registered.network),
        );

        match drain_on(&queued, registered.network).as_slice() {
            [HeadersMessage(headers)] => {
                assert_eq!(headers.payload.headers.len(), 4);
                assert_eq!(headers.payload.headers.last().unwrap().hash(), at);
            }
            got => panic!("expected four headers, got {got:?}"),
        }
    }

    #[test]
    fn headers_that_teach_us_nothing_are_not_answered_with_another_question() {
        let node = a_testnet_node();
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        let block = a_block_on(&node, 1);
        registered.take_block(block.clone()).unwrap();
        drain_on(&queued, registered.network);

        // A header we already have: nothing new, so nothing to ask for.
        deliver(
            &registered,
            &framed_on(
                Headers::new(vec![block.header().unwrap()]),
                registered.network,
            ),
        );

        assert!(
            drain_on(&queued, registered.network).is_empty(),
            "asking again on a batch that taught us nothing is how two nodes \
             talk forever"
        );
    }

    #[test]
    fn a_header_makes_us_ask_for_the_body_and_not_the_other_way_round() {
        let node = a_testnet_node();
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&registered);
        drain_on(&queued, registered.network);
        let block = a_block_on(&node, 1);
        let hash = block.header().unwrap().hash();

        deliver(
            &registered,
            &framed_on(
                Headers::new(vec![block.header().unwrap()]),
                registered.network,
            ),
        );

        match drain_on(&queued, registered.network).as_slice() {
            [GetdataMessage(getdata)] => {
                assert_eq!(getdata.payload.items, vec![Item::Block(hash)])
            }
            got => panic!("expected a getdata for the body, got {got:?}"),
        }
        assert_eq!(
            node.lock().unwrap().chain.height(),
            0,
            "a header is not a block"
        );
    }

    #[test]
    fn a_block_that_arrives_before_its_parent_makes_us_ask_for_the_parent() {
        let node = a_testnet_node();
        let (sender, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&sender);
        drain_on(&queued, sender.network);

        let first = a_block_on(&node, 1);
        let parent = first.header().unwrap().hash();
        sender.take_block(first).unwrap();
        let second = a_block_on(&node, 2);

        // A node that has neither: the second is an orphan there.
        let fresh = a_testnet_node();
        let (peer, asked) = a_registered_peer_of(&fresh, A_LISTEN_ADDRESS);
        identify(&peer);
        drain_on(&asked, peer.network);

        deliver(&peer, &framed_on(BlockMessage::new(second), peer.network));

        match drain_on(&asked, peer.network).as_slice() {
            [GetdataMessage(getdata)] => {
                assert_eq!(getdata.payload.items, vec![Item::Block(parent)])
            }
            got => panic!("expected a getdata for the parent, got {got:?}"),
        }
    }

    #[test]
    fn a_block_that_does_not_validate_is_not_offered_onward() {
        let node = a_testnet_node();
        let (sender, _queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        let (other, told) = a_registered_peer_of(&node, "127.0.0.1:5001");
        identify_as(&sender, 7, A_LISTEN_ADDRESS);
        identify_as(&other, 8, "127.0.0.1:5001");
        drain_on(&told, other.network);

        let mut forged = a_block_on(&node, 1);
        forged.transactions[0].outputs[0].value =
            crate::amount::Amount::from_atoms(crate::amount::subsidy(1).atoms() + 1).unwrap();
        assert!(forged.mine().unwrap());

        deliver(
            &sender,
            &framed_on(BlockMessage::new(forged), sender.network),
        );

        assert!(drain_on(&told, other.network).is_empty());
        assert_eq!(node.lock().unwrap().chain.height(), 0);
    }

    #[test]
    fn a_block_from_the_future_is_recorded_as_a_clock_problem() {
        let node = a_testnet_node();
        let (sender, _queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&sender);

        let mut early = a_block_on(&node, 1);
        early.time = now() + crate::difficulty::MAX_FUTURE_DRIFT + 60;
        assert!(early.mine().unwrap());

        deliver(
            &sender,
            &framed_on(BlockMessage::new(early), sender.network),
        );

        let said = node
            .lock()
            .unwrap()
            .log
            .recent()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(said.contains("REFUSING BLOCKS"), "{said}");
        assert!(said.contains("clock"), "{said}");
    }

    #[test]
    fn an_ordinary_block_refusal_is_not_shouted_about() {
        let node = a_testnet_node();
        let (sender, _queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        identify(&sender);

        let mut forged = a_block_on(&node, 1);
        forged.transactions[0].outputs[0].value =
            crate::amount::Amount::from_atoms(crate::amount::subsidy(1).atoms() + 1).unwrap();
        assert!(forged.mine().unwrap());

        deliver(
            &sender,
            &framed_on(BlockMessage::new(forged), sender.network),
        );

        let said = node
            .lock()
            .unwrap()
            .log
            .recent()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!said.contains("REFUSING BLOCKS"), "{said}");
    }

    #[rstest]
    #[case::inv(framed_inv(Txid::from_bytes([1; 32])))]
    #[case::getdata(framed_getdata(Txid::from_bytes([1; 32])))]
    fn relay_is_ignored_from_a_peer_that_has_not_identified_itself(#[case] message: Vec<u8>) {
        let node = a_testnet_node();
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        drain_on(&queued, registered.network);

        deliver(&registered, &message);

        assert!(
            drain_on(&queued, registered.network).is_empty(),
            "the gate is above these arms"
        );
    }

    #[test]
    fn a_transaction_from_a_peer_that_has_not_identified_itself_is_ignored() {
        let wallet = crate::wallet::Wallet::new();
        let node = a_node_funding(&wallet);
        let payment = a_payment(&node, &wallet);
        let (registered, queued) = a_registered_peer_of(&node, A_LISTEN_ADDRESS);
        drain_on(&queued, &crate::params::TESTNET);

        deliver(
            &registered,
            &framed_on(Tx::new(payment), &crate::params::TESTNET),
        );

        assert!(node.lock().unwrap().mempool.is_empty());
    }

    #[test]
    fn an_oversized_header_fails_the_connection_rather_than_being_awaited() {
        let (registered, queued) = a_registered_peer();
        let mut recv_buffer = Vec::new();
        let header = crate::messages::message::header_claiming(u32::MAX);

        let error = process_incoming_bytes(&registered, &mut recv_buffer, &header)
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

        let (registered, queued) = a_registered_peer();
        identify(&registered);
        while queued.try_recv().is_ok() {}

        let (ping, nonce) = framed_ping();
        let reader = InterruptsOnce {
            ping,
            interrupted: false,
        };

        read_loop(reader, &registered, NEVER)
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
            if let (Some(message), consumed) = MessageReceived::try_parse_message(buffer, &MAINNET)
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

    /// The next message answering something we sent, past what a connection
    /// says on its own: the keep-alive, and the getaddr on becoming Ready.
    fn next_reply(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> MessageReceived {
        loop {
            match next_message(stream, buffer) {
                PingMessage(_) | GetaddrMessage => continue,
                reply => return reply,
            }
        }
    }

    /// An address nothing is listening on, so a dial to it is refused at once.
    fn a_free_port() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        address
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

    fn eventually(mut settled: impl FnMut() -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);

        while Instant::now() < deadline {
            if settled() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }

        panic!("{what} within 5s");
    }

    /// What spawn_connection does, minus the registry, for tests about the
    /// thread pair rather than about the peer table.
    fn handle_alone(stream: TcpStream, peer_addr: SocketAddr, node: SharedNode) -> Result<()> {
        handle_alone_for(stream, peer_addr, node, HANDSHAKE_TIMEOUT)
    }

    fn handle_alone_for(
        stream: TcpStream,
        peer_addr: SocketAddr,
        node: SharedNode,
        handshake_timeout: Duration,
    ) -> Result<()> {
        let (outbound, queued) = mpsc::sync_channel(OUTBOUND_QUEUE);
        let registered = Registered::open(
            &node,
            peer_addr,
            Origin::Accepted,
            outbound,
            Arc::new(AtomicUsize::new(0)),
        )
        .expect("an empty table should accept a peer");
        handle_connection(
            stream,
            registered,
            queued,
            Arc::new(AtomicUsize::new(0)),
            handshake_timeout,
        )
    }

    /// A peer in a node's table, plus the queue its writer would drain.
    fn a_registered_peer() -> (Registered, Receiver<Vec<u8>>) {
        a_registered_peer_of(&a_node(), A_LISTEN_ADDRESS)
    }

    fn a_registered_peer_of(node: &SharedNode, from: &str) -> (Registered, Receiver<Vec<u8>>) {
        let (outbound, queued) = mpsc::sync_channel(OUTBOUND_QUEUE);
        let registered = Registered::open(
            node,
            from.parse().unwrap(),
            Origin::Accepted,
            outbound,
            Arc::new(AtomicUsize::new(0)),
        )
        .expect("an empty table should accept a peer");

        (registered, queued)
    }

    fn a_node() -> SharedNode {
        Node::shared(
            Config {
                mine: false,
                network: &MAINNET,
                host_address: "127.0.0.1:34352".parse().unwrap(),
                addresses_to_connect: Vec::new(),
            },
            &MAINNET.genesis().unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn a_connection_pings_its_peer_and_answers_the_peers_ping() {
        let (mut peer, accepted, peer_addr) = a_connected_pair();

        thread::spawn(move || handle_alone(accepted, peer_addr, a_node()));

        let mut buffer = Vec::new();
        assert!(
            matches!(next_message(&mut peer, &mut buffer), VersionMessage(_)),
            "a connection should open by identifying itself"
        );

        peer.write_all(&framed_version()).unwrap();
        assert!(matches!(next_reply(&mut peer, &mut buffer), VerackMessage));
        peer.write_all(&framed(Verack)).unwrap();

        assert!(
            matches!(next_message(&mut peer, &mut buffer), PingMessage(_)),
            "and ping it once it has identified itself"
        );
        assert!(
            matches!(next_message(&mut peer, &mut buffer), GetaddrMessage),
            "and ask it who else is out there"
        );
        assert!(
            matches!(next_message(&mut peer, &mut buffer), GetHeadersMessage(_)),
            "and whether we are behind it"
        );

        let (ping, nonce) = framed_ping();
        peer.write_all(&ping).unwrap();

        match next_reply(&mut peer, &mut buffer) {
            PongMessage(pong) => assert_eq!(nonce, pong.payload.nonce),
            other => panic!("expected a pong for our ping, got {other:?}"),
        }
    }

    #[test]
    fn the_version_a_connection_opens_with_carries_the_nodes_nonce_and_listen_address() {
        let (mut peer, accepted, peer_addr) = a_connected_pair();
        let node = a_node();
        let (nonce, listening_on) = {
            let node = node.lock().unwrap();
            (node.nonce, node.config.host_address)
        };

        thread::spawn(move || handle_alone(accepted, peer_addr, node));

        match next_message(&mut peer, &mut Vec::new()) {
            VersionMessage(version) => {
                assert_eq!(nonce, version.payload.nonce);
                assert_eq!(
                    listening_on, version.payload.listen_address,
                    "a peer re-dials the address we advertise, not the one it sees"
                );
            }
            other => panic!("expected a version, got {other:?}"),
        }
    }

    #[test]
    fn a_peer_that_exchanges_version_and_verack_over_a_socket_reaches_ready() {
        let (mut peer, accepted, _) = a_connected_pair();
        let node = a_node();
        let watched = Arc::clone(&node);

        spawn_connection(accepted, node, Origin::Accepted);

        let mut buffer = Vec::new();
        assert!(matches!(
            next_message(&mut peer, &mut buffer),
            VersionMessage(_)
        ));

        peer.write_all(&framed_version()).unwrap();
        // Past the keep-alive ping: gating that on Ready is #42's job, not this
        // ticket's, so it may legitimately arrive before the verack.
        assert!(
            matches!(next_reply(&mut peer, &mut buffer), VerackMessage),
            "a version must be answered with a verack"
        );

        peer.write_all(&framed(Verack)).unwrap();
        eventually(
            || {
                let node = watched.lock().unwrap();
                node.peers
                    .ids()
                    .first()
                    .and_then(|id| node.peers.handshake_of(*id))
                    .is_some_and(Handshake::is_ready)
            },
            "the peer never reached Ready after a completed handshake",
        );
    }

    #[test]
    fn a_version_is_answered_with_a_verack_and_only_their_verack_completes_it() {
        let (registered, queued) = a_registered_peer();
        let mut recv_buffer = Vec::new();

        process_incoming_bytes(&registered, &mut recv_buffer, &framed_version()).unwrap();

        assert!(
            !registered.is_ready(),
            "their version is half a handshake; ours is still unanswered"
        );
        let reply = queued.try_recv().expect("a version must be answered");
        assert!(
            matches!(parse_all(&reply).as_slice(), [VerackMessage]),
            "expected a verack, got {:?}",
            parse_all(&reply)
        );

        process_incoming_bytes(&registered, &mut recv_buffer, &framed(Verack)).unwrap();

        assert!(registered.is_ready());

        let queued: Vec<_> = std::iter::from_fn(|| queued.try_recv().ok())
            .flat_map(|framed| parse_all(&framed))
            .collect();

        assert!(
            matches!(
                queued.as_slice(),
                [PingMessage(_), GetaddrMessage, GetHeadersMessage(_)]
            ),
            "becoming Ready starts the keep-alive — the writer's timer would not \
             fire for a whole interval on its own — and asks who else is out \
             there, got {queued:?}"
        );
    }

    #[test]
    fn a_verack_before_any_version_is_refused() {
        let (registered, queued) = a_registered_peer();

        process_incoming_bytes(&registered, &mut Vec::new(), &framed(Verack))
            .expect_err("a verack answers a version this peer never sent");

        assert!(!registered.is_ready());
        assert!(queued.try_recv().is_err(), "nothing is owed to a bad peer");
    }

    #[test]
    fn a_second_version_after_the_handshake_is_a_protocol_error() {
        let (registered, queued) = a_registered_peer();
        identify(&registered);
        while queued.try_recv().is_ok() {}

        process_incoming_bytes(&registered, &mut Vec::new(), &framed_version())
            .expect_err("a handshake happens once; a second version is not a second one");

        assert!(
            queued.try_recv().is_err(),
            "a refused version must not be answered with another verack"
        );
    }

    #[test]
    fn a_peer_that_never_identifies_itself_loses_its_connection() {
        /// Connected and silent: every read expires the way a socket's read
        /// timeout does. It gives up eventually, so a node that stopped
        /// enforcing the deadline fails this test instead of hanging the suite.
        struct SaysNothing(usize);

        impl Read for SaysNothing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Ok(0);
                }
                self.0 -= 1;
                thread::sleep(Duration::from_millis(5));
                Err(std::io::Error::from(ErrorKind::WouldBlock))
            }
        }

        let (registered, _queued) = a_registered_peer();

        let error = read_loop(SaysNothing(40), &registered, Duration::from_millis(50))
            .expect_err("a peer that never identifies itself must not hold a slot forever");

        assert!(
            format!("{error:#}").contains("no handshake"),
            "got: {error:#}"
        );
    }

    #[test]
    fn a_peer_that_talks_without_identifying_itself_still_loses_its_connection() {
        /// Legal traffic, no handshake. Every read returns bytes, so the read
        /// timeout never expires and only an absolute deadline ends this. It
        /// runs out, so a node that lost the deadline fails rather than hangs.
        struct Chatters {
            pong: Vec<u8>,
            left: usize,
        }

        impl Read for Chatters {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.left == 0 {
                    return Ok(0);
                }
                self.left -= 1;
                thread::sleep(Duration::from_millis(2));
                buffer[..self.pong.len()].copy_from_slice(&self.pong);
                Ok(self.pong.len())
            }
        }

        let (registered, _queued) = a_registered_peer();
        let chatty = Chatters {
            pong: framed(Pong::new(Ping::new()).unwrap()),
            left: 100,
        };

        let error = read_loop(chatty, &registered, Duration::from_millis(50))
            .expect_err("the handshake deadline is absolute, not reset by every read");

        assert!(
            format!("{error:#}").contains("no handshake"),
            "got: {error:#}"
        );
    }

    #[test]
    fn a_peer_that_did_identify_itself_survives_a_read_that_expires() {
        struct ExpiresThenCloses {
            expired: bool,
        }

        impl Read for ExpiresThenCloses {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                if !self.expired {
                    self.expired = true;
                    return Err(std::io::Error::from(ErrorKind::WouldBlock));
                }
                Ok(0)
            }
        }

        let (registered, _queued) = a_registered_peer();
        identify(&registered);

        read_loop(
            ExpiresThenCloses { expired: false },
            &registered,
            Duration::from_millis(1),
        )
        .expect("a quiet established peer is not a peer that failed to hand shake");
    }

    #[test]
    fn a_connection_registers_a_peer_and_closing_it_removes_them() {
        let (peer, accepted, _) = a_connected_pair();
        let node = a_node();
        let watched = Arc::clone(&node);

        spawn_connection(accepted, node, Origin::Accepted);
        eventually(
            || watched.lock().unwrap().peers.len() == 1,
            "the connection never registered a peer",
        );

        drop(peer);
        eventually(
            || watched.lock().unwrap().peers.is_empty(),
            "the peer was still registered after its connection closed",
        );
    }

    #[test]
    fn dropping_a_peer_from_the_table_ends_its_connection() {
        let (mut peer, accepted, _) = a_connected_pair();
        let node = a_node();
        let watched = Arc::clone(&node);

        spawn_connection(accepted, node, Origin::Accepted);
        eventually(
            || watched.lock().unwrap().peers.len() == 1,
            "the connection never registered a peer",
        );

        // Removing the entry must take the connection with it, or the threads
        // and the peer's recv_buffer outlive the table meant to bound them.
        // This covers eviction with a drained queue; a full one is
        // a_stalled_write_ends_the_connection_rather_than_blocking_forever.
        let id = watched.lock().unwrap().peers.ids()[0];
        watched.lock().unwrap().peers.remove(id);

        let mut discarded = [0u8; 64];
        loop {
            match peer.read(&mut discarded) {
                Ok(0) => return,
                Ok(_) => continue,
                Err(e) => panic!("dropping a peer must close its socket, got {e}"),
            }
        }
    }

    #[test]
    fn what_a_connection_reports_reaches_the_nodes_log() {
        let (mut peer, accepted, _) = a_connected_pair();
        let node = a_node();
        let watched = Arc::clone(&node);

        spawn_connection(accepted, node, Origin::Accepted);
        peer.write_all(&[framed_version(), framed(Verack), framed_ping().0].concat())
            .unwrap();

        for expected in [
            "is handling a connection from",
            "Handshake with",
            "Ping received",
        ] {
            eventually(
                || {
                    watched
                        .lock()
                        .unwrap()
                        .log
                        .recent()
                        .any(|entry| entry.contains(expected))
                },
                &format!("{expected:?} never reached the log"),
            );
        }
    }

    #[rstest]
    #[case::every_slot_taken(crate::node::MAX_PEERS, Origin::Dialled)]
    #[case::inbound_share_taken(crate::node::MAX_INBOUND, Origin::Accepted)]
    fn a_refused_connection_leaves_the_table_as_it_found_it(
        #[case] fill: usize,
        #[case] origin: Origin,
    ) {
        let node = a_node();
        let mut held = Vec::new();

        for index in 0..fill {
            let (outbound, queued) = mpsc::sync_channel(OUTBOUND_QUEUE);
            held.push(queued);
            let filler = format!("127.0.0.1:{}", 5000 + index).parse().unwrap();
            node.lock()
                .unwrap()
                .peers
                .register(filler, origin, outbound, Arc::new(AtomicUsize::new(0)))
                .expect("the table should accept peers up to its bound");
        }

        let (mut peer, accepted, _) = a_connected_pair();
        spawn_connection(accepted, Arc::clone(&node), Origin::Accepted);

        let mut discarded = [0u8; 64];
        assert_eq!(
            0,
            peer.read(&mut discarded)
                .expect("the refusal should close, not hang"),
            "a refused peer should be hung up on, not left connected in silence"
        );
        assert_eq!(
            fill,
            node.lock().unwrap().peers.len(),
            "a refused connection must not displace an established peer"
        );
    }

    #[test]
    fn both_threads_end_when_the_peer_disconnects() {
        let (peer, accepted, peer_addr) = a_connected_pair();
        let (done, finished) = mpsc::channel();

        thread::spawn(move || {
            let _ = handle_alone(accepted, peer_addr, a_node());
            done.send(()).unwrap();
        });

        drop(peer);

        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("a connection whose peer is gone must not leave a thread parked");
    }

    #[test]
    fn losing_the_write_half_wakes_a_parked_reader() {
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
        // Not left to the read timeout: on an established connection that is
        // 20s away, and teardown cannot wait on it.
        drop(peer);
    }
}
