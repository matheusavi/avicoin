use secp256k1::hashes::{sha256, Hash};
use secp256k1::{rand, PublicKey, SecretKey};
use secp256k1::{Message, Secp256k1};
#[derive(Clone, Debug)]
pub struct Wallet {
    private_key: SecretKey,
    public_key: PublicKey,
}

impl Wallet {
    pub fn new() -> Self {
        let secp = Secp256k1::new();
        let (private_key, public_key) = secp.generate_keypair(&mut rand::rng());

        Wallet {
            private_key,
            public_key,
        }
    }

    pub fn get_available_balance() -> u64 {
        // TODO get from UTXO module
        10000000
    }
    pub fn send(amount: u64, destination_address: String) {
        // should be created before with public and private keys
        // look if there is available utxo
        // create a new transaction
    }
}
