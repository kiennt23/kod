//! The signed CLI's requests: HMAC-SHA256 of the body under the signer's
//! key, hex in the `x-kod-signature` header.
use sha2::{Digest, Sha256};

pub const SIGNATURE: &str = "x-kod-signature";

/// How far, in seconds, a request's `at` may be from the dispatcher's clock.
pub const WINDOW: i64 = 300;

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<u8>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(data)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

/// The signature the CLI sends for `body`.
pub fn sign(key: &[u8], body: &[u8]) -> String {
    hmac_sha256(key, body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compares in constant time.
pub fn verify(key: &[u8], body: &[u8], signature: &str) -> bool {
    let want = sign(key, body);
    want.len() == signature.len()
        && want
            .bytes()
            .zip(signature.bytes())
            .fold(0, |d, (a, b)| d | (a ^ b))
            == 0
}
