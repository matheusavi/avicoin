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
    fn the_handle_can_be_locked_from_a_connection_thread() {
        let node = Node::shared(config());

        let connection = {
            let node = Arc::clone(&node);
            thread::spawn(move || node.lock().unwrap().config.host_address)
        };

        let observed = connection.join().unwrap();
        let expected = node.lock().unwrap().config.host_address;

        assert_eq!(
            expected, observed,
            "a connection thread reads node state through the shared handle"
        );
    }
}
