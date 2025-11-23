use crate::transaction::{Transaction, TxIn, TxOut};
use anyhow::anyhow;
use secp256k1::hashes::{sha256, Hash};
use secp256k1::{rand, PublicKey, SecretKey};
use secp256k1::{Message, Secp256k1};
use std::io::ErrorKind;

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

    pub fn send(amount: u64, fee: u64, destination_address: String) -> Result<Transaction> {
        if amount + fee > Self::get_available_balance() {
            return Err(anyhow!("Insufficient funds"));
        }

        // get available utxo
        let outpoints = Self::get_outpoints(amount, fee);

        let sig = Secp256k1::signing_only();

        let mut inputs = Vec::new();

        for outpoint in outpoints {
            inputs.push(TxIn {
                previous_output: outpoint,
                signature: sig.sign_ecdsa(outpoint.tx_id, Self.private_key),
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
