//! SHA-256 helpers for `xtask package`. The on-disk `SHA256SUMS.txt` follows the
//! GNU coreutils `sha256sum` format — lowercase hex, two spaces, filename — so a
//! consumer can verify the download with the ubiquitous `sha256sum -c
//! SHA256SUMS.txt`. (Earlier builds emitted a bare uppercase hash with no
//! filename, matching PowerShell `Get-FileHash`; that shape was non-standard and
//! is replaced here. No stable release had shipped, so there are no consumers of
//! the old format to break.)

use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Lowercase hex SHA-256 of `bytes`, matching `sha256sum` output.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The `SHA256SUMS.txt` body in coreutils format: one `"{hash}  {filename}\n"`
/// line per `(hash, filename)` entry (two spaces = text mode). Verifiable with
/// `sha256sum -c`.
pub fn sha256sums_body(entries: &[(String, String)]) -> String {
    let mut out = String::new();
    for (hash, name) in entries {
        let _ = writeln!(out, "{hash}  {name}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_the_known_sha256_of_abc() {
        // NIST FIPS 180-2 test vector for "abc" (lowercase, as sha256sum prints).
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn empty_input_hashes_to_the_known_empty_digest() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn body_is_coreutils_format_hash_two_spaces_filename() {
        let body = sha256sums_body(&[(
            sha256_hex(b"abc"),
            "find-my-files-v0.1.0-win-x64.zip".to_owned(),
        )]);
        assert_eq!(
            body,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  \
             find-my-files-v0.1.0-win-x64.zip\n"
        );
    }

    #[test]
    fn body_lists_every_entry_one_per_line() {
        let body = sha256sums_body(&[
            (sha256_hex(b"abc"), "a.zip".to_owned()),
            (sha256_hex(b""), "b.json".to_owned()),
        ]);
        assert_eq!(body.lines().count(), 2);
        assert!(body.ends_with("  b.json\n"));
    }
}
