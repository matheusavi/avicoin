use crate::block::Block;
use crate::config::Config;
use crate::mempool::Mempool;
use crate::utxo::UtxoSet;
use anyhow::{Context, Result};
use rand::Rng;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

pub const MAX_PEERS: usize = 32;
// ADR-0018.
pub const RESERVED_OUTBOUND: usize = 8;
pub const MAX_INBOUND: usize = MAX_PEERS - RESERVED_OUTBOUND;
pub const OUTBOUND_QUEUE: usize = 128;

pub type PeerId = u64;
pub type SharedNode = Arc<Mutex<Node>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Dialled,
    Accepted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Handshake {
    #[default]
    AwaitingVersion,
    AwaitingVerack,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeEvent {
    Version,
    Verack,
}

impl Handshake {
    pub fn advance(self, event: HandshakeEvent) -> Result<Handshake, anyhow::Error> {
        match (self, event) {
            (Handshake::AwaitingVersion, HandshakeEvent::Version) => Ok(Handshake::AwaitingVerack),
            (Handshake::AwaitingVerack, HandshakeEvent::Verack) => Ok(Handshake::Ready),
            (Handshake::AwaitingVersion, HandshakeEvent::Verack) => {
                Err(anyhow::anyhow!("verack before any version"))
            }
            (_, HandshakeEvent::Version) => Err(anyhow::anyhow!("version after the handshake")),
            (_, HandshakeEvent::Verack) => Err(anyhow::anyhow!("verack after the handshake")),
        }
    }

    pub fn is_ready(self) -> bool {
        self == Handshake::Ready
    }
}

#[derive(Debug)]
pub struct PeerHandle {
    pub address: SocketAddr,
    pub origin: Origin,
    pub handshake: Handshake,
    pub nonce: Option<u64>,
    pub listening: Option<SocketAddr>,
    outbound: SyncSender<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    AtCapacity,
    InboundFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    New,
    Ourselves,
    AlreadyConnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivered {
    Yes,
    NotReady,
    Gone,
}

#[derive(Debug, Default)]
pub struct PeerTable {
    peers: HashMap<PeerId, PeerHandle>,
    next_id: PeerId,
}

impl PeerTable {
    pub fn register(
        &mut self,
        address: SocketAddr,
        origin: Origin,
        outbound: SyncSender<Vec<u8>>,
    ) -> Result<PeerId, Refused> {
        if self.peers.len() >= MAX_PEERS {
            return Err(Refused::AtCapacity);
        }

        if origin == Origin::Accepted && self.count(Origin::Accepted) >= MAX_INBOUND {
            return Err(Refused::InboundFull);
        }

        let id = self.next_id;
        self.next_id += 1;
        self.peers.insert(
            id,
            PeerHandle {
                address,
                origin,
                handshake: Handshake::default(),
                nonce: None,
                listening: None,
                outbound,
            },
        );

        Ok(id)
    }

    fn count(&self, origin: Origin) -> usize {
        self.peers
            .values()
            .filter(|peer| peer.origin == origin)
            .count()
    }

    fn holding(&self, nonce: u64) -> Option<PeerId> {
        self.peers
            .iter()
            .find(|(_, peer)| peer.nonce == Some(nonce))
            .map(|(id, _)| *id)
    }

    fn origin_of(&self, id: PeerId) -> Option<Origin> {
        self.peers.get(&id).map(|peer| peer.origin)
    }

    fn nonce_of(&self, id: PeerId) -> Option<u64> {
        self.peers.get(&id).and_then(|peer| peer.nonce)
    }

    fn identify(
        &mut self,
        id: PeerId,
        nonce: u64,
        listening: SocketAddr,
        survivor: Origin,
    ) -> Identity {
        if let Some(held) = self.holding(nonce).filter(|held| *held != id) {
            if self.origin_of(id) != Some(survivor) {
                return Identity::AlreadyConnected;
            }
            self.remove(held);
        }

        if let Some(peer) = self.peers.get_mut(&id) {
            peer.nonce = Some(nonce);
            peer.listening = Some(listening);
        }

        Identity::New
    }

    pub fn listening_addresses(&self, except: PeerId) -> Vec<SocketAddr> {
        self.peers
            .iter()
            .filter(|(id, peer)| **id != except && peer.handshake.is_ready())
            .filter_map(|(_, peer)| peer.listening)
            .collect()
    }

    pub fn listening_of(&self, id: PeerId) -> Option<SocketAddr> {
        self.peers.get(&id).and_then(|peer| peer.listening)
    }

    pub fn knows(&self, address: SocketAddr) -> bool {
        self.peers
            .values()
            .any(|peer| peer.listening == Some(address) || peer.address == address)
    }

    pub fn has_room(&self) -> bool {
        self.peers.len() < MAX_PEERS
    }

    pub fn remove(&mut self, id: PeerId) -> Option<PeerHandle> {
        self.peers.remove(&id)
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Read and write in one lock: the transition depends on the current state,
    /// so splitting it would let two messages race the same peer forward.
    pub fn advance_handshake(
        &mut self,
        id: PeerId,
        event: HandshakeEvent,
    ) -> Result<Handshake, anyhow::Error> {
        let peer = self
            .peers
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("no such peer"))?;

        peer.handshake = peer.handshake.advance(event)?;
        Ok(peer.handshake)
    }

    pub fn handshake_of(&self, id: PeerId) -> Option<Handshake> {
        self.peers.get(&id).map(|peer| peer.handshake)
    }

    pub fn ids(&self) -> Vec<PeerId> {
        self.peers.keys().copied().collect()
    }

    pub fn send_to(&mut self, id: PeerId, message: Vec<u8>) -> Delivered {
        match self.handshake_of(id) {
            Some(handshake) if handshake.is_ready() => self.queue(id, message),
            Some(_) => Delivered::NotReady,
            None => Delivered::Gone,
        }
    }

    /// The only send that may precede Ready, and only from the one state that
    /// needs it: our `verack` is what carries the peer out of `AwaitingVerack`,
    /// so gating it on Ready would gate it on itself.
    pub fn answer_handshake(&mut self, id: PeerId, message: Vec<u8>) -> Delivered {
        match self.handshake_of(id) {
            Some(Handshake::AwaitingVerack) => self.queue(id, message),
            Some(_) => Delivered::NotReady,
            None => Delivered::Gone,
        }
    }

    /// `try_send`, because a blocking send would hold the node's lock on one
    /// stalled socket and stop delivery to everyone else.
    fn queue(&mut self, id: PeerId, message: Vec<u8>) -> Delivered {
        let Some(peer) = self.peers.get(&id) else {
            return Delivered::Gone;
        };

        match peer.outbound.try_send(message) {
            Ok(()) => Delivered::Yes,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.peers.remove(&id);
                Delivered::Gone
            }
        }
    }

    pub fn broadcast(&mut self, message: &[u8]) -> usize {
        self.relay(message, None)
    }

    /// Every Ready peer but one — the peer a piece of news is *about* has no
    /// use for it, and would only dial itself.
    pub fn relay(&mut self, message: &[u8], except: Option<PeerId>) -> usize {
        let mut delivered = 0;

        for id in self.ids() {
            if Some(id) != except && self.send_to(id, message.to_vec()) == Delivered::Yes {
                delivered += 1;
            }
        }

        delivered
    }
}

pub const LOG_CAPACITY: usize = 512;

#[derive(Debug)]
pub struct Log {
    entries: VecDeque<String>,
    capacity: usize,
}

impl Log {
    fn new(capacity: usize) -> Self {
        Log {
            entries: VecDeque::new(),
            capacity,
        }
    }

    fn push(&mut self, entry: String) {
        self.entries.push_back(entry);
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }

    pub fn recent(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }
}

impl Default for Log {
    fn default() -> Self {
        Log::new(LOG_CAPACITY)
    }
}

#[derive(Debug)]
pub struct Node {
    pub config: Config,
    pub peers: PeerTable,
    pub log: Log,
    /// Minted once per run so a node can recognise a connection to itself.
    pub nonce: u64,
    /// Discovery dials that have not finished connecting — ADR-0017.
    pub dialling: usize,
    pub utxo: UtxoSet,
    pub mempool: Mempool,
}

impl Node {
    /// The genesis coinbase enters the UTXO set by the path any other coinbase
    /// takes; there is no second way in — ADR-0007.
    pub fn shared(config: Config, genesis: &Block) -> Result<SharedNode> {
        let mut utxo = UtxoSet::new();
        for transaction in &genesis.transactions {
            utxo.connect(transaction, 0)
                .context("seeding the UTXO set from the genesis block")?;
        }

        Ok(Arc::new(Mutex::new(Self {
            config,
            peers: PeerTable::default(),
            log: Log::default(),
            nonce: rand::rng().next_u64(),
            dialling: 0,
            utxo,
            mempool: Mempool::new(),
        })))
    }

    pub fn identify(&mut self, id: PeerId, nonce: u64, listening: SocketAddr) -> Identity {
        if nonce == self.nonce {
            return Identity::Ourselves;
        }

        self.peers
            .identify(id, nonce, listening, survivor_of(self.nonce, nonce))
    }
}

/// ADR-0015: over the nonces, not from here, or both ends of a mutual dial drop
/// what the other kept.
fn survivor_of(ours: u64, theirs: u64) -> Origin {
    if ours > theirs {
        Origin::Dialled
    } else {
        Origin::Accepted
    }
}

/// Never call while already holding the node lock: std's `Mutex` is not
/// reentrant and the borrow checker will not stop you.
pub fn record(node: &SharedNode, entry: impl Into<String>) {
    let entry = entry.into();

    // stdout first, and outside the lock. Printing is a blocking syscall, so
    // holding the node across it would stall every peer behind a slow pipe —
    // and a write that fails would poison the mutex rather than just the line.
    println!("{entry}");

    // Logging must not be the thing that kills a thread, so a poisoned lock is
    // recovered rather than propagated.
    node.lock()
        .unwrap_or_else(|held| held.into_inner())
        .log
        .push(entry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::sync::mpsc::{sync_channel, Receiver};
    use std::thread;

    #[test]
    fn a_node_starts_holding_exactly_the_allocation_its_network_derives() {
        use crate::params::{MAINNET, TESTNET};

        let mainnet = Node::shared(config(), &MAINNET.genesis().unwrap()).unwrap();
        let testnet = Node::shared(config(), &TESTNET.genesis().unwrap()).unwrap();

        assert!(
            mainnet.lock().unwrap().utxo.is_empty(),
            "mainnet has no premine"
        );
        assert_eq!(
            testnet.lock().unwrap().utxo.len(),
            TESTNET.allocation().unwrap().len()
        );
    }

    #[test]
    fn the_allocation_is_reachable_by_outpoint_like_any_other_coin() {
        use crate::params::TESTNET;

        let genesis = TESTNET.genesis().unwrap();
        let node = Node::shared(config(), &genesis).unwrap();
        let coinbase = &genesis.transactions[0];
        let held = node.lock().unwrap();

        for index in 0..coinbase.outputs.len() {
            let outpoint = crate::transaction::Outpoint {
                txid: coinbase.get_tx_id(),
                v_out: index as u32,
            };

            let coin = held.utxo.get(&outpoint).expect("a real txid and index");
            assert_eq!(coin.height, 0);
            assert!(coin.from_coinbase);
        }
    }

    fn test_node() -> SharedNode {
        Node::shared(config(), &crate::params::MAINNET.genesis().unwrap()).unwrap()
    }

    fn config() -> Config {
        Config {
            network: &crate::params::MAINNET,
            host_address: "127.0.0.1:34352".parse().unwrap(),
            addresses_to_connect: Vec::new(),
        }
    }

    fn address(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    fn a_node_with_nonce(nonce: u64) -> Node {
        Node {
            utxo: UtxoSet::new(),
            mempool: Mempool::new(),
            config: config(),
            peers: PeerTable::default(),
            log: Log::default(),
            nonce,
            dialling: 0,
        }
    }

    fn a_ready_peer(table: &mut PeerTable, port: u16) -> (PeerId, Receiver<Vec<u8>>) {
        let (id, queued) = a_peer(table, port);
        shake_hands(table, id);
        (id, queued)
    }

    fn shake_hands(table: &mut PeerTable, id: PeerId) {
        for event in [HandshakeEvent::Version, HandshakeEvent::Verack] {
            table
                .advance_handshake(id, event)
                .expect("a fresh peer should complete a handshake");
        }
    }

    fn a_peer(table: &mut PeerTable, port: u16) -> (PeerId, Receiver<Vec<u8>>) {
        a_peer_from(table, port, Origin::Accepted)
    }

    fn a_peer_from(
        table: &mut PeerTable,
        port: u16,
        origin: Origin,
    ) -> (PeerId, Receiver<Vec<u8>>) {
        let (outbound, queued) = sync_channel(OUTBOUND_QUEUE);
        let id = table
            .register(address(port), origin, outbound)
            .expect("registering the first peers should succeed");
        (id, queued)
    }

    fn a_full_table() -> (PeerTable, Vec<Receiver<Vec<u8>>>) {
        let mut table = PeerTable::default();
        let queues = (0..MAX_PEERS)
            .map(|index| a_peer_from(&mut table, 5000 + index as u16, Origin::Dialled).1)
            .collect();

        (table, queues)
    }

    #[test]
    fn every_connection_thread_holds_the_same_node_not_a_copy() {
        let node = test_node();

        let threads: Vec<_> = (0..4)
            .map(|_| {
                let node = Arc::clone(&node);
                thread::spawn(move || {
                    let address = node.lock().unwrap().config.host_address;
                    (node, address)
                })
            })
            .collect();

        for thread in threads {
            let (observed, address) = thread.join().unwrap();

            assert!(
                Arc::ptr_eq(&node, &observed),
                "a connection thread must share the node, not receive a copy of it"
            );
            assert_eq!(node.lock().unwrap().config.host_address, address);
        }
    }

    #[test]
    fn the_log_evicts_the_oldest_entries_and_keeps_the_rest_in_order() {
        let mut log = Log::new(3);

        for entry in 1..=5 {
            log.push(format!("entry {entry}"));
        }

        assert_eq!(
            vec!["entry 3", "entry 4", "entry 5"],
            log.recent().collect::<Vec<_>>(),
            "the buffer keeps the most recent entries, oldest first"
        );
        assert_eq!(3, log.recent().count(), "it never grows past its capacity");
    }

    #[test]
    fn a_log_shorter_than_its_capacity_evicts_nothing() {
        let mut log = Log::new(3);
        log.push("only entry".to_string());

        assert_eq!(vec!["only entry"], log.recent().collect::<Vec<_>>());
    }

    #[test]
    fn the_default_log_holds_the_capacity_the_glossary_documents() {
        // A literal, not LOG_CAPACITY: asserting the constant against itself
        // moves with any change to it and proves nothing.
        assert_eq!(512, LOG_CAPACITY);

        let mut log = Log::default();
        for entry in 0..LOG_CAPACITY + 10 {
            log.push(entry.to_string());
        }

        assert_eq!(512, log.recent().count());
        assert_eq!(Some("10"), log.recent().next(), "the first ten are evicted");
    }

    #[test]
    fn recording_reaches_the_nodes_log() {
        let node = test_node();

        record(&node, "something happened");
        record(&node, format!("and then {}", "something else"));

        assert_eq!(
            vec!["something happened", "and then something else"],
            node.lock().unwrap().log.recent().collect::<Vec<_>>()
        );
    }

    #[test]
    fn recording_from_a_connection_thread_reaches_the_same_log() {
        let node = test_node();

        let writer = Arc::clone(&node);
        thread::spawn(move || record(&writer, "from another thread"))
            .join()
            .unwrap();

        assert_eq!(
            vec!["from another thread"],
            node.lock().unwrap().log.recent().collect::<Vec<_>>(),
            "every connection thread logs into the one shared buffer"
        );
    }

    #[test]
    fn a_registered_peer_is_listed_until_it_is_removed() {
        let mut table = PeerTable::default();
        assert!(table.is_empty());

        let (id, _queued) = a_peer(&mut table, 5000);
        assert_eq!(1, table.len());
        assert_eq!(vec![id], table.ids());

        assert!(table.remove(id).is_some());
        assert!(
            table.is_empty(),
            "closing a connection must remove its peer"
        );
        assert!(table.remove(id).is_none(), "removing twice is not a peer");
        assert!(table.ids().is_empty());
    }

    #[test]
    fn broadcast_reaches_every_peer() {
        let mut table = PeerTable::default();
        let queues: Vec<_> = (0..3)
            .map(|index| a_ready_peer(&mut table, 5000 + index).1)
            .collect();

        assert_eq!(3, table.broadcast(b"a block"));

        for queued in &queues {
            assert_eq!(b"a block".to_vec(), queued.try_recv().unwrap());
            assert!(queued.try_recv().is_err(), "one broadcast, one message");
        }
    }

    #[test]
    fn send_to_reaches_exactly_one_peer() {
        let mut table = PeerTable::default();
        let (first, to_first) = a_ready_peer(&mut table, 5000);
        let (_, to_second) = a_ready_peer(&mut table, 5001);

        assert_eq!(
            Delivered::Yes,
            table.send_to(first, b"just for you".to_vec())
        );

        assert_eq!(b"just for you".to_vec(), to_first.try_recv().unwrap());
        assert!(
            to_second.try_recv().is_err(),
            "send_to must not reach anyone else"
        );
    }

    #[test]
    fn broadcast_passes_over_a_peer_still_shaking_hands() {
        let mut table = PeerTable::default();
        let (_, to_ready) = a_ready_peer(&mut table, 5000);
        let (_, to_halfway) = a_peer(&mut table, 5001);

        assert_eq!(
            1,
            table.broadcast(b"a block"),
            "it reports who it reached, not who it holds"
        );

        assert_eq!(b"a block".to_vec(), to_ready.try_recv().unwrap());
        assert!(
            to_halfway.try_recv().is_err(),
            "a peer that has not identified itself gets nothing"
        );
        assert_eq!(2, table.len(), "and is not dropped for it");
    }

    #[test]
    fn a_peer_becoming_ready_starts_receiving_broadcasts() {
        let mut table = PeerTable::default();
        let (id, queued) = a_peer(&mut table, 5000);

        assert_eq!(0, table.broadcast(b"too early"));

        shake_hands(&mut table, id);

        assert_eq!(1, table.broadcast(b"right on time"));
        assert_eq!(
            vec![b"right on time".to_vec()],
            std::iter::from_fn(|| queued.try_recv().ok()).collect::<Vec<_>>(),
            "nothing from before Ready may have been buffered for later"
        );
    }

    #[test]
    fn send_to_a_peer_still_shaking_hands_delivers_nothing_and_keeps_it() {
        let mut table = PeerTable::default();
        let (id, queued) = a_peer(&mut table, 5000);

        assert_eq!(
            Delivered::NotReady,
            table.send_to(id, b"too early".to_vec())
        );

        assert!(queued.try_recv().is_err());
        assert_eq!(1, table.len(), "refusing to send is not refusing the peer");
    }

    #[test]
    fn the_handshakes_own_reply_goes_out_before_the_peer_is_ready() {
        let mut table = PeerTable::default();
        let (id, queued) = a_peer(&mut table, 5000);
        table
            .advance_handshake(id, HandshakeEvent::Version)
            .unwrap();

        assert_eq!(
            Delivered::Yes,
            table.answer_handshake(id, b"a verack".to_vec())
        );
        assert_eq!(b"a verack".to_vec(), queued.try_recv().unwrap());
    }

    #[rstest]
    #[case::before_their_version(Handshake::AwaitingVersion)]
    #[case::after_the_handshake(Handshake::Ready)]
    fn the_handshake_door_opens_only_for_the_state_that_needs_it(#[case] state: Handshake) {
        let mut table = PeerTable::default();
        let (id, queued) = a_peer(&mut table, 5000);
        if state == Handshake::Ready {
            shake_hands(&mut table, id);
        }

        // An escape hatch wide enough for any state is not an exception, it is
        // the gate with a second way round it.
        assert_eq!(
            Delivered::NotReady,
            table.answer_handshake(id, b"not a verack".to_vec())
        );
        assert!(queued.try_recv().is_err());
    }

    #[test]
    fn send_to_an_unknown_peer_delivers_nothing() {
        let mut table = PeerTable::default();
        let (_, to_first) = a_ready_peer(&mut table, 5000);

        assert_eq!(Delivered::Gone, table.send_to(404, b"nobody".to_vec()));
        assert!(to_first.try_recv().is_err());
    }

    #[test]
    fn a_peer_that_never_drains_does_not_hold_up_the_others() {
        let mut table = PeerTable::default();
        let (stalled, never_drained) = a_ready_peer(&mut table, 5000);
        let (_, to_second) = a_ready_peer(&mut table, 5001);
        let (_, to_third) = a_ready_peer(&mut table, 5002);

        for _ in 0..OUTBOUND_QUEUE {
            assert_eq!(Delivered::Yes, table.send_to(stalled, b"backlog".to_vec()));
        }

        assert_eq!(
            2,
            table.broadcast(b"a block"),
            "the two healthy peers must still be reached"
        );
        assert_eq!(b"a block".to_vec(), to_second.try_recv().unwrap());
        assert_eq!(b"a block".to_vec(), to_third.try_recv().unwrap());

        assert_eq!(2, table.len(), "the stalled peer is dropped, not buffered");
        drop(never_drained);
    }

    #[test]
    fn a_peers_queue_does_not_grow_past_the_bound() {
        let mut table = PeerTable::default();
        let (stalled, never_drained) = a_ready_peer(&mut table, 5000);

        for queued_so_far in 0..OUTBOUND_QUEUE {
            assert_eq!(
                Delivered::Yes,
                table.send_to(stalled, b"backlog".to_vec()),
                "the bound is {OUTBOUND_QUEUE}, so message {queued_so_far} should fit"
            );
        }

        assert_eq!(
            Delivered::Gone,
            table.send_to(stalled, b"one too many".to_vec()),
            "a queue past {OUTBOUND_QUEUE} is unbounded buffering, not backpressure"
        );
        assert!(table.is_empty(), "a peer that cannot keep up is dropped");

        let drained = std::iter::from_fn(|| never_drained.try_recv().ok()).count();
        assert_eq!(
            OUTBOUND_QUEUE, drained,
            "exactly the bound should have been buffered"
        );
    }

    #[test]
    fn a_peer_whose_writer_is_gone_is_dropped() {
        let mut table = PeerTable::default();
        let (id, queued) = a_ready_peer(&mut table, 5000);
        drop(queued);

        assert_eq!(
            Delivered::Gone,
            table.send_to(id, b"into the void".to_vec())
        );
        assert!(table.is_empty(), "a peer with no writer is not a peer");
    }

    #[test]
    fn an_address_alone_no_longer_decides_whether_two_connections_are_one_peer() {
        let mut table = PeerTable::default();

        let mut queues = Vec::new();

        for _ in 0..2 {
            let (outbound, queued) = sync_channel(OUTBOUND_QUEUE);
            queues.push(queued);
            table
                .register(address(5000), Origin::Dialled, outbound)
                .expect("an address says nothing about identity now");
        }

        assert_eq!(
            2,
            table.len(),
            "dedup waits for the nonce; refusing on an address would drop a \
             second peer behind one NAT, and never catch one dialling us back"
        );
    }

    #[test]
    fn a_version_carrying_our_own_nonce_means_we_dialled_ourselves() {
        let mut node = a_node_with_nonce(77);
        let (id, _queued) = a_peer(&mut node.peers, 5000);

        assert_eq!(Identity::Ourselves, node.identify(id, 77, address(9000)));
        assert_eq!(
            None,
            node.peers.nonce_of(id),
            "a connection to ourselves is never claimed as a peer"
        );
    }

    #[test]
    fn a_peer_is_remembered_by_the_nonce_it_gave() {
        let mut node = a_node_with_nonce(77);
        let (id, _queued) = a_peer(&mut node.peers, 5000);

        assert_eq!(Identity::New, node.identify(id, 1234, address(9000)));
        assert_eq!(Some(1234), node.peers.nonce_of(id));
    }

    #[test]
    fn identifying_one_connection_twice_does_not_make_it_its_own_duplicate() {
        let mut node = a_node_with_nonce(77);
        let (id, _queued) = a_peer(&mut node.peers, 5000);

        node.identify(id, 1234, address(9000));

        assert_eq!(Identity::New, node.identify(id, 1234, address(9000)));
        assert_eq!(1, node.peers.len(), "it must not evict itself");
    }

    /// One node's view of a mutual dial: the same peer on two connections, one
    /// we dialled and one we accepted, each identifying itself in turn. Returns
    /// the origin of the connection left standing.
    fn mutual_dial(ours: u64, theirs: u64, order: [Origin; 2]) -> Origin {
        let mut node = a_node_with_nonce(ours);
        let mut queues = Vec::new();
        let mut ids = Vec::new();

        for origin in [Origin::Dialled, Origin::Accepted] {
            let (outbound, queued) = sync_channel(OUTBOUND_QUEUE);
            queues.push(queued);
            ids.push((
                origin,
                node.peers
                    .register(address(5000), origin, outbound)
                    .unwrap(),
            ));
        }

        for wanted in order {
            let id = ids.iter().find(|(o, _)| *o == wanted).unwrap().1;
            if node.peers.origin_of(id).is_none() {
                continue;
            }
            // Standing in for the connection thread, which hangs up on that
            // answer and takes the peer out of the table with it.
            if node.identify(id, theirs, address(9000)) == Identity::AlreadyConnected {
                node.peers.remove(id);
            }
        }

        let left = node.peers.ids();
        assert_eq!(1, left.len(), "a mutual dial must settle on one connection");
        node.peers.origin_of(left[0]).unwrap()
    }

    #[rstest]
    #[case::dialled_identifies_first([Origin::Dialled, Origin::Accepted])]
    #[case::accepted_identifies_first([Origin::Accepted, Origin::Dialled])]
    fn both_ends_of_a_mutual_dial_keep_the_same_socket(#[case] order: [Origin; 2]) {
        let at_higher = mutual_dial(10, 5, order);
        let at_lower = mutual_dial(5, 10, order);

        assert_ne!(
            at_higher, at_lower,
            "both ends kept their own dial, which is two different sockets"
        );
        assert_eq!(
            Origin::Dialled,
            at_higher,
            "the tie-break is the larger nonce's dial"
        );
    }

    #[test]
    fn each_node_mints_its_own_nonce() {
        assert_ne!(
            test_node().lock().unwrap().nonce,
            test_node().lock().unwrap().nonce,
            "a shared nonce cannot tell a self-connection from a peer"
        );
    }

    #[test]
    fn a_peer_counts_only_once_both_sides_have_identified_themselves() {
        let mut table = PeerTable::default();
        let (id, _queued) = a_peer(&mut table, 5000);

        assert_eq!(Some(Handshake::AwaitingVersion), table.handshake_of(id));

        assert_eq!(
            Handshake::AwaitingVerack,
            table
                .advance_handshake(id, HandshakeEvent::Version)
                .unwrap(),
            "their version is half of it: ours is still unanswered"
        );
        assert_eq!(
            Handshake::Ready,
            table.advance_handshake(id, HandshakeEvent::Verack).unwrap()
        );
        assert!(table.handshake_of(id).unwrap().is_ready());
    }

    #[rstest]
    #[case::verack_first(Handshake::AwaitingVersion, HandshakeEvent::Verack)]
    #[case::two_versions(Handshake::AwaitingVerack, HandshakeEvent::Version)]
    #[case::two_veracks(Handshake::Ready, HandshakeEvent::Verack)]
    #[case::version_after_ready(Handshake::Ready, HandshakeEvent::Version)]
    fn a_handshake_out_of_order_is_refused_rather_than_restarted(
        #[case] state: Handshake,
        #[case] event: HandshakeEvent,
    ) {
        state
            .advance(event)
            .expect_err("the handshake happens once, in one order");
    }

    #[test]
    fn a_peer_that_left_the_table_cannot_advance_a_handshake() {
        let mut table = PeerTable::default();
        let (id, _queued) = a_peer(&mut table, 5000);
        table.remove(id);

        assert!(table
            .advance_handshake(id, HandshakeEvent::Version)
            .is_err());
        assert_eq!(None, table.handshake_of(id));
    }

    #[test]
    fn what_we_can_tell_a_peer_about_is_where_the_others_listen() {
        let mut node = a_node_with_nonce(77);
        let (asker, _a) = a_peer(&mut node.peers, 40001);
        let (other, _o) = a_peer(&mut node.peers, 40002);
        shake_hands(&mut node.peers, asker);
        shake_hands(&mut node.peers, other);
        node.peers
            .identify(other, 1, address(8333), Origin::Accepted);

        let served = node.peers.listening_addresses(asker);

        assert_eq!(
            vec![address(8333)],
            served,
            "the port a peer dialled us from is not one anybody can dial back"
        );
    }

    #[test]
    fn a_peer_that_has_not_said_where_it_listens_is_not_offered_to_anyone() {
        let mut table = PeerTable::default();
        let (asker, _a) = a_peer(&mut table, 40001);
        let (halfway, _h) = a_peer(&mut table, 40002);
        shake_hands(&mut table, halfway);

        assert!(
            table.listening_addresses(asker).is_empty(),
            "a peer whose version has not arrived has told us nothing to pass on"
        );
    }

    #[test]
    fn a_peer_is_never_offered_itself() {
        let mut node = a_node_with_nonce(77);
        let (only, _queued) = a_peer(&mut node.peers, 40001);
        shake_hands(&mut node.peers, only);
        node.peers
            .identify(only, 1, address(8333), Origin::Accepted);

        assert!(node.peers.listening_addresses(only).is_empty());
    }

    #[test]
    fn a_peer_is_known_by_either_the_address_it_dialled_from_or_the_one_it_listens_on() {
        let mut node = a_node_with_nonce(77);
        let (id, _queued) = a_peer(&mut node.peers, 40001);
        node.peers.identify(id, 1, address(8333), Origin::Accepted);

        assert!(node.peers.knows(address(8333)), "its listening address");
        assert!(node.peers.knows(address(40001)), "the one it reached us on");
        assert!(!node.peers.knows(address(9999)));
    }

    #[test]
    fn a_full_table_has_no_room_for_a_discovered_address() {
        let mut table = PeerTable::default();
        let mut queues = Vec::new();

        for index in 0..MAX_PEERS - 1 {
            queues.push(a_peer_from(&mut table, 5000 + index as u16, Origin::Dialled).1);
        }
        assert!(table.has_room(), "one short of the cap is still room");

        queues.push(a_peer_from(&mut table, 6000, Origin::Dialled).1);

        assert!(!table.has_room());
    }

    #[test]
    fn inbound_connections_cannot_take_the_slots_kept_for_dialling() {
        let mut table = PeerTable::default();
        let mut queues = Vec::new();

        for index in 0..MAX_INBOUND {
            queues.push(a_peer(&mut table, 5000 + index as u16).1);
        }

        let (outbound, _refused) = sync_channel(OUTBOUND_QUEUE);
        assert_eq!(
            Err(Refused::InboundFull),
            table.register(address(6000), Origin::Accepted, outbound),
            "an attacker who can fill every slot decides who this node sees"
        );

        let (outbound, _dialled) = sync_channel(OUTBOUND_QUEUE);
        assert!(
            table
                .register(address(6001), Origin::Dialled, outbound)
                .is_ok(),
            "the point of the reservation is that dialling still works"
        );
    }

    #[test]
    fn a_node_nobody_dials_out_to_still_fills_most_of_its_table() {
        let mut table = PeerTable::default();
        let mut queues = Vec::new();

        for index in 0..MAX_INBOUND {
            queues.push(a_peer(&mut table, 5000 + index as u16).1);
        }

        // Against the table's own size, not against MAX_INBOUND, which would be
        // the constant asserted against itself and would move with any change
        // to it. The reservation may cost a listen-only node slots; it may not
        // cost it most of them.
        assert!(
            table.len() > MAX_PEERS / 2,
            "{} of {MAX_PEERS} slots left to a node nobody dials out to",
            table.len()
        );
    }

    #[test]
    fn the_reservation_is_the_size_the_documents_say() {
        // Literals: asserting MAX_INBOUND == MAX_PEERS - RESERVED_OUTBOUND
        // proves arithmetic, and moves silently with whatever it is measuring.
        assert_eq!(32, MAX_PEERS);
        assert_eq!(8, RESERVED_OUTBOUND);
        assert_eq!(24, MAX_INBOUND);
    }

    #[test]
    fn dialled_peers_may_use_the_whole_table() {
        let (table, _queues) = a_full_table();

        assert_eq!(MAX_PEERS, table.len());
        assert_eq!(MAX_PEERS, table.count(Origin::Dialled));
    }

    #[test]
    fn a_full_table_refuses_the_next_peer() {
        let (mut table, _queues) = a_full_table();
        assert_eq!(MAX_PEERS, table.len());

        let (outbound, _refused) = sync_channel(OUTBOUND_QUEUE);
        assert_eq!(
            Err(Refused::AtCapacity),
            table.register(address(6000), Origin::Dialled, outbound),
            "the policy is to refuse the newcomer, not to evict an established peer"
        );
        assert_eq!(MAX_PEERS, table.len());
    }
}
