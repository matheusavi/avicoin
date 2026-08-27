use crate::address::Address;
use crate::amount::Amount;
use crate::block::Block;
use crate::crypto::PrivateKey;
use crate::script::p2pkh;
use crate::transaction::{Outpoint, Transaction, TxIn, TxOut, Witness};
use anyhow::{anyhow, Context, Result};

impl std::fmt::Debug for Params {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} network", self.name)
    }
}

/// One network's consensus-relevant values. A node picks a set at startup and
/// never edits a field of it, so a test chain and the public chain differ from
/// block zero rather than only at the wire.
pub struct Params {
    pub name: &'static str,
    pub magic: [u8; 4],
    pub starting_bits: u32,
    /// How often the retarget rule wants a block. Thirty seconds on the public
    /// chain (ADR-0006); one on the test network, where the point is that a
    /// test finishes rather than that a week holds a halving.
    pub target_block_time: u32,
    pub maturity: u32,
    pub genesis_time: u32,
    pub genesis_nonce: u32,
    pub genesis_message: &'static str,
    allocation: &'static str,
}

pub type Network = &'static Params;

pub static MAINNET: Params = Params {
    name: "main",
    magic: *b"AVI1",
    starting_bits: 0x1e00ffff,
    target_block_time: 30,
    maturity: 100,
    genesis_time: 1_756_252_800,
    genesis_nonce: 3_378_221,
    genesis_message: "Avi Coin: a chain you can read in an afternoon",
    allocation: include_str!("../params/mainnet.allocation"),
};

pub static TESTNET: Params = Params {
    name: "test",
    magic: *b"AVIT",
    starting_bits: 0x2000ffff,
    target_block_time: 1,
    maturity: 1,
    genesis_time: 1_756_252_800,
    genesis_nonce: 15,
    genesis_message: "Avi Coin test network",
    allocation: include_str!("../params/testnet.allocation"),
};

pub const NETWORKS: [Network; 2] = [&MAINNET, &TESTNET];

pub fn by_name(name: &str) -> Result<Network> {
    NETWORKS
        .into_iter()
        .find(|network| network.name == name)
        .ok_or_else(|| {
            anyhow!(
                "{name:?} is not a network; try one of {:?}",
                NETWORKS.map(|network| network.name)
            )
        })
}

impl Params {
    pub fn allocation(&self) -> Result<Vec<(Address, Amount)>> {
        self.allocation
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(parse_allocation_line)
            .collect()
    }

    /// The allocation *is* the genesis coinbase's outputs, so a premined coin
    /// is an ordinary output with a real txid and index. There is no path that
    /// puts a coin in the UTXO set without one — ADR-0007.
    pub fn genesis(&self) -> Result<Block> {
        let mut block = self.genesis_candidate()?;
        block.nonce = self.genesis_nonce;
        block.seal().with_context(|| {
            format!(
                "the {} genesis block does not satisfy its own proof of work; \
                 regenerate its nonce with `cargo test regenerate_genesis_nonces -- --ignored`",
                self.name
            )
        })?;

        Ok(block)
    }

    fn genesis_candidate(&self) -> Result<Block> {
        let mut coinbase_data = 0u32.to_le_bytes().to_vec();
        coinbase_data.extend(self.genesis_message.as_bytes());

        let outputs = self
            .allocation()?
            .into_iter()
            .map(|(address, value)| TxOut {
                value,
                script_pubkey: p2pkh(&address.pubkey_hash()),
            })
            .collect();

        let coinbase = Transaction {
            version: 1,
            inputs: vec![TxIn {
                previous_output: Outpoint::null(),
                coinbase_data,
                witness: Witness::empty(),
            }],
            outputs,
        };

        Ok(Block::new(
            1,
            [0; 32],
            self.genesis_time,
            self.starting_bits,
            vec![coinbase],
        ))
    }

    pub fn genesis_hash(&self) -> Result<[u8; 32]> {
        self.genesis()?
            .hash
            .context("a sealed block always has a hash")
    }
}

fn parse_allocation_line(line: &str) -> Result<(Address, Amount)> {
    let (address, atoms) = line
        .split_once(char::is_whitespace)
        .ok_or_else(|| anyhow!("{line:?} is not \"<address> <atoms>\""))?;

    Ok((
        address.parse().with_context(|| format!("in {line:?}"))?,
        Amount::from_atoms(
            atoms
                .trim()
                .parse()
                .with_context(|| format!("in {line:?}"))?,
        )?,
    ))
}

/// The private keys behind the test allocation, shipped so a test has coins to
/// spend at height zero. Public knowledge by construction.
pub fn test_keys() -> Result<Vec<PrivateKey>> {
    include_str!("../params/testnet.keys")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let material: [u8; 32] = hex::decode(line)
                .context("a test key is 32 hex bytes")?
                .try_into()
                .map_err(|_| anyhow!("a test key is 32 hex bytes"))?;
            PrivateKey::parse(&material)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Address;

    /// ADR-0007 asks for a tool that regenerates the nonce when an allocation
    /// changes. This is it: run it, paste what it prints into the statics
    /// above. Ignored because it mines.
    #[test]
    fn every_networks_genesis_satisfies_its_own_proof_of_work() {
        for network in NETWORKS {
            network
                .genesis()
                .unwrap_or_else(|error| panic!("{}: {error:#}", network.name));
        }
    }

    #[test]
    fn a_genesis_whose_nonce_was_not_regenerated_refuses_to_seal() {
        let stale = Params {
            genesis_nonce: MAINNET.genesis_nonce.wrapping_add(1),
            ..copy_of(&MAINNET)
        };

        let error = format!("{:#}", stale.genesis().unwrap_err());

        assert!(error.contains("proof of work"), "{error}");
        assert!(error.contains("regenerate"), "{error}");
    }

    fn copy_of(params: &'static Params) -> Params {
        Params {
            name: params.name,
            magic: params.magic,
            starting_bits: params.starting_bits,
            target_block_time: params.target_block_time,
            maturity: params.maturity,
            genesis_time: params.genesis_time,
            genesis_nonce: params.genesis_nonce,
            genesis_message: params.genesis_message,
            allocation: params.allocation,
        }
    }

    #[test]
    fn two_networks_do_not_share_a_chain() {
        assert_ne!(
            MAINNET.genesis_hash().unwrap(),
            TESTNET.genesis_hash().unwrap(),
            "differing parameters must differ from block zero, not only at the wire"
        );
        assert_ne!(MAINNET.magic, TESTNET.magic);
    }

    #[test]
    fn mainnet_has_no_premine() {
        assert!(MAINNET.allocation().unwrap().is_empty());
        assert!(MAINNET.genesis().unwrap().transactions[0]
            .outputs
            .is_empty());
    }

    #[test]
    fn the_test_allocation_funds_the_keys_that_ship_with_it() {
        let funded: Vec<Address> = TESTNET
            .allocation()
            .unwrap()
            .into_iter()
            .map(|(address, _)| address)
            .collect();
        let keys = test_keys().unwrap();

        assert!(!keys.is_empty());
        for key in keys {
            let address = Address::for_public_key(&key.public_key());
            assert!(
                funded.contains(&address),
                "{address} ships a private key and holds nothing"
            );
        }
    }

    #[test]
    fn a_genesis_coinbase_is_shaped_like_any_other_coinbase() {
        let genesis = TESTNET.genesis().unwrap();
        let coinbase = &genesis.transactions[0];

        assert_eq!(genesis.transactions.len(), 1);
        assert_eq!(coinbase.inputs.len(), 1);
        assert!(coinbase.inputs[0].previous_output.is_null());
        assert!(coinbase.inputs[0].witness.items().is_empty());
        assert_eq!(&coinbase.inputs[0].coinbase_data[..4], &0u32.to_le_bytes());
    }

    #[test]
    fn a_coinbase_data_field_stays_within_its_hundred_bytes() {
        for network in NETWORKS {
            let genesis = network.genesis().unwrap();

            assert!(genesis.transactions[0].inputs[0].coinbase_data.len() <= 100);
        }
    }

    #[test]
    fn an_allocation_that_does_not_parse_is_an_error_rather_than_a_silent_skip() {
        assert!(parse_allocation_line("not-an-address 5").is_err());
        assert!(parse_allocation_line("ASsJFYneQdx4S1qKnESF3U6ko7mQFY3jmV").is_err());
        assert!(parse_allocation_line("ASsJFYneQdx4S1qKnESF3U6ko7mQFY3jmV nine").is_err());
    }

    #[test]
    fn a_network_is_chosen_by_name_and_an_unknown_one_is_refused() {
        assert_eq!(by_name("main").unwrap().name, "main");
        assert_eq!(by_name("test").unwrap().name, "test");
        assert!(by_name("regtest").is_err());
    }

    /// ADR-0007 asks for a tool that regenerates the nonce when an allocation
    /// changes. This is it: run it, paste what it prints into the statics
    /// above. Ignored because it mines.
    #[test]
    #[ignore]
    fn regenerate_genesis_nonces() {
        for network in NETWORKS {
            let mut block = network.genesis_candidate().unwrap();
            assert!(block.mine().unwrap(), "no nonce solves this header");

            println!("{}: genesis_nonce: {},", network.name, block.nonce);
        }
    }
}
