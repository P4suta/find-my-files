//! SHA-256 helpers for `xtask package`. The on-disk format intentionally
//! matches the previous `(Get-FileHash …).Hash | Out-File -Encoding ascii`
//! output — uppercase hex on a single line — so the published SHA256SUMS.txt
//! is byte-for-byte the same shape consumers already saw.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Uppercase hex SHA-256 of `bytes`, matching PowerShell `Get-FileHash`.
pub fn sha256_upper_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02X}");
    }
    s
}

/// The SHA256SUMS.txt body: the uppercase hash on a single newline-terminated
/// line (matching the old `.Hash | Out-File` output).
pub fn sha256sums_body(hash_upper_hex: &str) -> String {
    format!("{hash_upper_hex}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_the_known_sha256_of_abc() {
        // NIST FIPS 180-2 test vector for "abc".
        assert_eq!(
            sha256_upper_hex(b"abc"),
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        );
    }

    #[test]
    fn empty_input_hashes_to_the_known_empty_digest() {
        assert_eq!(
            sha256_upper_hex(b""),
            "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"
        );
    }

    #[test]
    fn body_is_uppercase_hash_plus_single_newline() {
        let body = sha256sums_body(&sha256_upper_hex(b"abc"));
        assert_eq!(
            body,
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD\n"
        );
    }
}
