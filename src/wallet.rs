use secp256k1::hashes::{sha256, Hash};
use secp256k1::{rand, PublicKey, SecretKey};
use secp256k1::{Message, Secp256k1};
#[derive(Clone, Debug)]
pub struct Wallet {
    private_key: SecretKey,
    public_key: PublicKey,
}

impl Wallet {
    // these are the required public methods
    // create wallet
    // get_availableamount
    // send transaction
    pub fn new() -> Self {
        let secp = Secp256k1::new();
        let (private_key, public_key) = secp.generate_keypair(&mut rand::rng());

        Wallet {
            private_key,
            public_key,
        }
    }
}
