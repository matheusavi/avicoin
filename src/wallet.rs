use crate::crypto::{PrivateKey, PublicKey, Signature};

#[derive(Clone)]
pub struct Wallet {
    private_key: PrivateKey,
}

impl Wallet {
    pub fn new() -> Self {
        Wallet {
            private_key: PrivateKey::random(),
        }
    }

    pub fn public_key(&self) -> PublicKey {
        self.private_key.public_key()
    }

    pub fn sign(&self, digest: &[u8; 32]) -> Signature {
        self.private_key.sign(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::verify;

    #[test]
    fn a_wallet_signs_with_the_key_it_publishes() {
        let wallet = Wallet::new();
        let digest = [7u8; 32];

        assert!(verify(&wallet.sign(&digest), &digest, &wallet.public_key()));
    }

    #[test]
    fn two_wallets_do_not_share_a_key() {
        assert_ne!(Wallet::new().public_key(), Wallet::new().public_key());
    }
}
