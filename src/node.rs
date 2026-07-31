use crate::config::Config;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

/// Every connection owns a `recv_buffer` that a peer may legally hold at
/// `MAX_PAYLOAD_SIZE`, so the node's exposure is this many times 32 MiB.
/// Lowering the per-connection ceiling is what would make that small; this only
/// stops the multiplier being chosen by whoever dials us.
pub const MAX_PEERS: usize = 32;

/// Messages queued for one peer before it is considered unable to keep up.
pub const OUTBOUND_QUEUE: usize = 128;

pub type PeerId = u64;
pub type SharedNode = Arc<Mutex<Node>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Dialled,
    Accepted,
}

#[derive(Debug)]
pub struct PeerHandle {
    pub address: SocketAddr,
    pub origin: Origin,
    outbound: SyncSender<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    AtCapacity,
    AlreadyDialled,
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
        if origin == Origin::Dialled && self.dialled(address) {
            return Err(Refused::AlreadyDialled);
        }

        if self.peers.len() >= MAX_PEERS {
            return Err(Refused::AtCapacity);
        }

        let id = self.next_id;
        self.next_id += 1;
        self.peers.insert(
            id,
            PeerHandle {
                address,
                origin,
                outbound,
            },
        );

        Ok(id)
    }

    fn dialled(&self, address: SocketAddr) -> bool {
        self.peers
            .values()
            .any(|peer| peer.origin == Origin::Dialled && peer.address == address)
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

    pub fn addresses(&self) -> Vec<SocketAddr> {
        self.peers.values().map(|peer| peer.address).collect()
    }

    /// Delivers to one peer, dropping it if it cannot take the message.
    pub fn send_to(&mut self, id: PeerId, message: Vec<u8>) -> bool {
        let Some(peer) = self.peers.get(&id) else {
            return false;
        };

        match peer.outbound.try_send(message) {
            Ok(()) => true,
            Err(_) => {
                self.peers.remove(&id);
                false
            }
        }
    }

    /// Delivers to every peer, dropping those that cannot take the message.
    ///
    /// `try_send` rather than `send`: a blocking send would let one stalled
    /// socket hold the node's lock and stop delivery to everyone else, which is
    /// the whole reason the writer thread exists.
    pub fn broadcast(&mut self, message: &[u8]) -> usize {
        let mut delivered = 0;
        let mut failed = Vec::new();

        for (id, peer) in &self.peers {
            match peer.outbound.try_send(message.to_vec()) {
                Ok(()) => delivered += 1,
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => failed.push(*id),
            }
        }

        for id in failed {
            self.peers.remove(&id);
        }

        delivered
    }
}

#[derive(Debug)]
pub struct Node {
    pub config: Config,
    pub peers: PeerTable,
}

impl Node {
    pub fn shared(config: Config) -> SharedNode {
        Arc::new(Mutex::new(Self {
            config,
            peers: PeerTable::default(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{sync_channel, Receiver};
    use std::thread;

    fn config() -> Config {
        Config {
            host_address: "127.0.0.1:34352".parse().unwrap(),
            addresses_to_connect: Vec::new(),
        }
    }

    fn address(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    fn a_peer(table: &mut PeerTable, port: u16) -> (PeerId, Receiver<Vec<u8>>) {
        let (outbound, queued) = sync_channel(OUTBOUND_QUEUE);
        let id = table
            .register(address(port), Origin::Accepted, outbound)
            .expect("registering the first peers should succeed");
        (id, queued)
    }

    #[test]
    fn every_connection_thread_holds_the_same_node_not_a_copy() {
        let node = Node::shared(config());

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
    fn a_registered_peer_is_listed_until_it_is_removed() {
        let mut table = PeerTable::default();
        assert!(table.is_empty());

        let (id, _queued) = a_peer(&mut table, 5000);
        assert_eq!(1, table.len());
        assert_eq!(vec![address(5000)], table.addresses());

        assert!(table.remove(id).is_some());
        assert!(
            table.is_empty(),
            "closing a connection must remove its peer"
        );
        assert!(table.remove(id).is_none(), "removing twice is not a peer");
    }

    #[test]
    fn broadcast_reaches_every_peer() {
        let mut table = PeerTable::default();
        let queues: Vec<_> = (0..3)
            .map(|index| a_peer(&mut table, 5000 + index).1)
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
        let (first, to_first) = a_peer(&mut table, 5000);
        let (_, to_second) = a_peer(&mut table, 5001);

        assert!(table.send_to(first, b"just for you".to_vec()));

        assert_eq!(b"just for you".to_vec(), to_first.try_recv().unwrap());
        assert!(
            to_second.try_recv().is_err(),
            "send_to must not reach anyone else"
        );
    }

    #[test]
    fn send_to_an_unknown_peer_delivers_nothing() {
        let mut table = PeerTable::default();
        let (_, to_first) = a_peer(&mut table, 5000);

        assert!(!table.send_to(404, b"nobody".to_vec()));
        assert!(to_first.try_recv().is_err());
    }

    #[test]
    fn a_peer_that_never_drains_does_not_hold_up_the_others() {
        let mut table = PeerTable::default();
        let (stalled, never_drained) = a_peer(&mut table, 5000);
        let (_, to_second) = a_peer(&mut table, 5001);
        let (_, to_third) = a_peer(&mut table, 5002);

        for _ in 0..OUTBOUND_QUEUE {
            assert!(table.send_to(stalled, b"backlog".to_vec()));
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
        let (stalled, never_drained) = a_peer(&mut table, 5000);

        for queued_so_far in 0..OUTBOUND_QUEUE {
            assert!(
                table.send_to(stalled, b"backlog".to_vec()),
                "the bound is {OUTBOUND_QUEUE}, so message {queued_so_far} should fit"
            );
        }

        assert!(
            !table.send_to(stalled, b"one too many".to_vec()),
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
        let (id, queued) = a_peer(&mut table, 5000);
        drop(queued);

        assert!(!table.send_to(id, b"into the void".to_vec()));
        assert!(table.is_empty(), "a peer with no writer is not a peer");
    }

    #[test]
    fn dialling_the_same_address_twice_registers_one_peer() {
        let mut table = PeerTable::default();
        let (outbound, _first) = sync_channel(OUTBOUND_QUEUE);
        table
            .register(address(5000), Origin::Dialled, outbound)
            .unwrap();

        let (outbound, _second) = sync_channel(OUTBOUND_QUEUE);
        assert_eq!(
            Err(Refused::AlreadyDialled),
            table.register(address(5000), Origin::Dialled, outbound)
        );
        assert_eq!(1, table.len());
    }

    #[test]
    fn a_peer_that_dialled_us_is_not_confused_with_one_we_dialled() {
        let mut table = PeerTable::default();
        let (outbound, _dialled) = sync_channel(OUTBOUND_QUEUE);
        table
            .register(address(5000), Origin::Dialled, outbound)
            .unwrap();

        // An accepted connection shows an ephemeral source port, so it cannot be
        // matched against a listen address until M2's version nonce exists.
        let (outbound, _accepted) = sync_channel(OUTBOUND_QUEUE);
        assert!(table
            .register(address(5000), Origin::Accepted, outbound)
            .is_ok());
        assert_eq!(2, table.len());
    }

    #[test]
    fn a_full_table_refuses_the_next_peer() {
        let mut table = PeerTable::default();
        let mut queues = Vec::new();

        for index in 0..MAX_PEERS {
            queues.push(a_peer(&mut table, 5000 + index as u16).1);
        }
        assert_eq!(MAX_PEERS, table.len());

        let (outbound, _refused) = sync_channel(OUTBOUND_QUEUE);
        assert_eq!(
            Err(Refused::AtCapacity),
            table.register(address(6000), Origin::Accepted, outbound),
            "the policy is to refuse the newcomer, not to evict an established peer"
        );
        assert_eq!(MAX_PEERS, table.len());
    }
}
