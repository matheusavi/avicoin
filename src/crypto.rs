use anyhow::{anyhow, Result};
use k256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use k256::ecdsa::{Signature as EcdsaSignature, SigningKey, VerifyingKey};
use rand::Rng;

pub const PUBLIC_KEY_LEN: usize = 33;
pub const SIGNATURE_LEN: usize = 64;

#[derive(Clone)]
pub struct PrivateKey(SigningKey);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PublicKey([u8; PUBLIC_KEY_LEN]);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Signature([u8; SIGNATURE_LEN]);

impl PrivateKey {
    pub fn random() -> Self {
        let mut material = [0u8; 32];
        loop {
            rand::rng().fill_bytes(&mut material);
            if let Ok(key) = SigningKey::from_slice(&material) {
                return PrivateKey(key);
            }
        }
    }

    pub fn public_key(&self) -> PublicKey {
        let point = self.0.verifying_key().to_sec1_point(true);
        let mut bytes = [0u8; PUBLIC_KEY_LEN];
        bytes.copy_from_slice(point.as_bytes());
        PublicKey(bytes)
    }

    pub fn sign(&self, digest: &[u8; 32]) -> Signature {
        let signature: EcdsaSignature = self
            .0
            .sign_prehash(digest)
            .expect("a 32-byte digest is always signable");
        let signature = signature.normalize_s();
        Signature(signature.to_bytes().into())
    }
}

impl PublicKey {
    pub fn parse(bytes: &[u8]) -> Result<PublicKey> {
        let bytes: [u8; PUBLIC_KEY_LEN] = bytes.try_into().map_err(|_| {
            anyhow!(
                "a public key is {PUBLIC_KEY_LEN} bytes, got {}",
                bytes.len()
            )
        })?;

        VerifyingKey::from_sec1_bytes(&bytes)
            .map_err(|_| anyhow!("not a point on the curve"))
            .map(|_| PublicKey(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.0
    }
}

impl Signature {
    pub fn parse(bytes: &[u8]) -> Result<Signature> {
        let bytes: [u8; SIGNATURE_LEN] = bytes
            .try_into()
            .map_err(|_| anyhow!("a signature is {SIGNATURE_LEN} bytes, got {}", bytes.len()))?;

        let signature =
            EcdsaSignature::from_slice(&bytes).map_err(|_| anyhow!("not a valid (r, s) pair"))?;

        if signature.normalize_s() != signature {
            return Err(anyhow!("signature is not low-S"));
        }

        Ok(Signature(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; SIGNATURE_LEN] {
        &self.0
    }
}

pub fn verify(signature: &Signature, digest: &[u8; 32], public_key: &PublicKey) -> bool {
    let Ok(key) = VerifyingKey::from_sec1_bytes(&public_key.0) else {
        return false;
    };
    let Ok(signature) = EcdsaSignature::from_slice(&signature.0) else {
        return false;
    };

    key.verify_prehash(digest, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::Signature as EcdsaSignature;

    fn digest(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn high_s(signature: &Signature) -> [u8; SIGNATURE_LEN] {
        let parsed = EcdsaSignature::from_slice(signature.as_bytes()).unwrap();
        let (r, s) = parsed.split_scalars();
        EcdsaSignature::from_scalars(*r, -*s)
            .unwrap()
            .to_bytes()
            .into()
    }

    #[test]
    fn a_public_key_is_thirty_three_compressed_bytes() {
        let key = PrivateKey::random().public_key();

        assert_eq!(key.as_bytes().len(), PUBLIC_KEY_LEN);
        assert!(matches!(key.as_bytes()[0], 0x02 | 0x03));
    }

    #[test]
    fn a_signature_is_sixty_four_bytes_of_r_and_s() {
        let key = PrivateKey::random();

        assert_eq!(key.sign(&digest(1)).as_bytes().len(), SIGNATURE_LEN);
    }

    #[test]
    fn a_signature_verifies_against_the_digest_and_key_that_made_it() {
        let key = PrivateKey::random();
        let signature = key.sign(&digest(1));

        assert!(verify(&signature, &digest(1), &key.public_key()));
    }

    #[test]
    fn a_signature_does_not_verify_against_another_digest() {
        let key = PrivateKey::random();
        let signature = key.sign(&digest(1));

        assert!(!verify(&signature, &digest(2), &key.public_key()));
    }

    #[test]
    fn a_signature_does_not_verify_against_another_key() {
        let signature = PrivateKey::random().sign(&digest(1));

        assert!(!verify(
            &signature,
            &digest(1),
            &PrivateKey::random().public_key()
        ));
    }

    #[test]
    fn signing_never_produces_a_high_s_signature() {
        for seed in 0..16u8 {
            let signature = PrivateKey::random().sign(&digest(seed));

            assert!(Signature::parse(signature.as_bytes()).is_ok());
        }
    }

    #[test]
    fn the_high_s_twin_of_a_valid_signature_is_refused() {
        let key = PrivateKey::random();
        let signature = key.sign(&digest(1));
        let twin = high_s(&signature);

        assert_ne!(&twin, signature.as_bytes());
        assert!(Signature::parse(&twin).is_err());
    }

    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let key = PrivateKey::random();
        let signature = key.sign(&digest(1));

        assert!(Signature::parse(&signature.as_bytes()[..63]).is_err());
        assert!(Signature::parse(&[signature.as_bytes().as_slice(), &[0]].concat()).is_err());
    }

    #[test]
    fn an_uncompressed_public_key_is_refused() {
        let key = PrivateKey::random();
        let uncompressed = k256::ecdsa::VerifyingKey::from_sec1_bytes(key.public_key().as_bytes())
            .unwrap()
            .to_sec1_point(false);

        assert_eq!(uncompressed.as_bytes().len(), 65);
        assert!(PublicKey::parse(uncompressed.as_bytes()).is_err());
    }

    #[test]
    fn thirty_three_bytes_that_are_not_a_curve_point_are_refused() {
        let past_the_field = [0xff; PUBLIC_KEY_LEN - 1];

        assert!(PublicKey::parse(&[&[0x02], past_the_field.as_slice()].concat()).is_err());
    }

    #[test]
    fn a_public_key_round_trips_through_its_bytes() {
        let key = PrivateKey::random().public_key();

        assert_eq!(PublicKey::parse(key.as_bytes()).unwrap(), key);
    }
}
