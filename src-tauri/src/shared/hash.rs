// SPDX-License-Identifier: MIT
//! SHA-256 fingerprints, including the digest of a streamed file.
use sha2::{Digest, Sha256};

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(&Sha256::digest(bytes))
}

pub(crate) fn digest_hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
