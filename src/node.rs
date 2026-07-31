use crate::config::Config;
use std::sync::{Arc, Mutex};

pub type SharedNode = Arc<Mutex<Node>>;

#[derive(Debug)]
pub struct Node {
    pub config: Config,
}

impl Node {
    pub fn shared(config: Config) -> SharedNode {
        Arc::new(Mutex::new(Self { config }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn config() -> Config {
        Config {
            host_address: "127.0.0.1:34352".parse().unwrap(),
            addresses_to_connect: Vec::new(),
        }
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
}
