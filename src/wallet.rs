use crate::transaction::{Outpoint, Transaction, TxIn, TxOut};
use anyhow::{anyhow, Result};
use secp256k1::hashes::{sha256, Hash};
use secp256k1::{rand, Message, PublicKey, SecretKey};
use secp256k1::Secp256k1;

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
        // TODO: get from UTXO module
        10000000
    }

    pub fn get_outpoints(amount: u64, fee: u64) -> Vec<Outpoint> {
        // TODO: implement UTXO selection logic
        vec![
            Outpoint{
                tx_id: [0; 32],
                v_out: 0,
            }
        ]
    }

    pub fn send(&self, amount: u64, fee: u64, destination_address: String) -> Result<Transaction> {
        if amount + fee > Self::get_available_balance() {
            return Err(anyhow!("Insufficient funds"));
        }

        // get available utxo
        let outpoints = Self::get_outpoints(amount, fee);

        let secp = Secp256k1::signing_only();
        let mut inputs = Vec::new();

        for outpoint in outpoints {
            // Create a message from the transaction hash
            let msg_hash = sha256::Hash::hash(&outpoint.tx_id);
            let message = Message::from_digest(msg_hash.to_byte_array());
            
            let signature = secp.sign_ecdsa(message, &self.private_key);
            
            inputs.push(TxIn {
                previous_output: outpoint,
                signature: signature.to_string(),
                sequence: 0xFFFFFFFF,
            })
        }

        // create change

        // create a new transaction
        Ok(Transaction {
            version: 1,
            inputs,
            outputs: vec![TxOut {
                value: amount,
                destiny_pub_key: destination_address,
            }],
            lock_time: 0,
        })
    }
}
