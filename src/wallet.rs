use crate::transaction::{Outpoint, Transaction, TxIn, TxOut};
use anyhow::{anyhow, Result};
use secp256k1::Secp256k1;
use secp256k1::{rand, Message, PublicKey, SecretKey};

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
    pub fn send(&self, amount: u64, fee: u64, destination_address: String) -> Result<Transaction> {
        if amount + fee > Self::get_available_balance() {
            return Err(anyhow!("Insufficient funds"));
        }

        // get available utxo
        let outpoints = Self::get_outpoints(amount, fee);

        let secp = Secp256k1::signing_only();
        let mut inputs = Vec::new();

        for outpoint in outpoints {
            // TODO base on current tx instead
            let signature =
                secp.sign_ecdsa(Message::from_digest(outpoint.tx_id), &self.private_key);

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

    fn get_outpoints(amount: u64, fee: u64) -> Vec<Outpoint> {
        // TODO: implement UTXO selection logic
        vec![Outpoint {
            tx_id: [0; 32],
            v_out: 0,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_send_creates_valid_transaction() {
        let wallet = Wallet::new();
        let amount = 5000;
        let fee = 100;
        let destination = "destination_address_123".to_string();

        let result = wallet.send(amount, fee, destination.clone());

        assert!(result.is_ok());
        let tx = result.unwrap();
        assert_eq!(tx.version, 1);
        assert_eq!(tx.outputs[0].value, amount);
        assert_eq!(tx.outputs[0].destiny_pub_key, destination);
    }

    #[test]
    fn test_send_fails_with_insufficient_funds() {
        let wallet = Wallet::new();
        let amount = 9000000;
        let fee = 1000001; // Total exceeds available balance
        let destination = "destination_address_123".to_string();

        let result = wallet.send(amount, fee, destination);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Insufficient funds");
    }
}
