//! The ed25519 key an image is signed with, as `imgtool`'s
//! `keys/ed25519.py` uses one.

use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

use crate::Error;

/// DER of an ed25519 `SubjectPublicKeyInfo` up to the 32 key bytes
/// (RFC 8410).
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// An ed25519 private key.
#[derive(Clone)]
pub struct Key(SigningKey);

impl Key {
    /// The file `imgtool keygen -t ed25519` writes: PKCS#8 PEM, unencrypted.
    pub fn from_pem(pem: &str) -> Result<Self, Error> {
        SigningKey::from_pkcs8_pem(pem)
            .map(Self)
            .map_err(|e| Error::Key(e.to_string()))
    }

    /// The key with this RFC 8032 seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    /// The 32 public key bytes.
    pub fn public(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    /// The `KEYHASH` TLV: SHA-256 of the public key's
    /// `SubjectPublicKeyInfo` DER, which is what the bootloader embeds
    /// (`imgtool getpub`) and hashes.
    pub fn keyhash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(SPKI_PREFIX);
        hasher.update(self.public());
        hasher.finalize().into()
    }

    /// The `ED25519` TLV: the signature of the image's SHA-256 digest, not
    /// of the image.
    pub fn sign(&self, digest: &[u8; 32]) -> [u8; 64] {
        self.0.sign(digest).to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_what_is_not_a_private_key_pem() {
        assert!(matches!(Key::from_pem(""), Err(Error::Key(_))));
        // A public key, as `imgtool getpub -e pem` would write.
        let public = "-----BEGIN PUBLIC KEY-----\n\
            MCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=\n\
            -----END PUBLIC KEY-----\n";
        assert!(matches!(Key::from_pem(public), Err(Error::Key(_))));
    }

    /// RFC 8032 section 7.1, test 1.
    #[test]
    fn rfc8032_public_key() {
        let seed = [
            0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec,
            0x2c, 0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03,
            0x1c, 0xae, 0x7f, 0x60,
        ];
        assert_eq!(
            Key::from_seed(&seed).public(),
            [
                0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64,
                0x07, 0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68,
                0xf7, 0x07, 0x51, 0x1a
            ]
        );
    }
}
