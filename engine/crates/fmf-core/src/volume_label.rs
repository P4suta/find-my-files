//! Canonical drive-letter volume identity at every privileged OS boundary.

#![forbid(unsafe_code)]

use std::fmt;

/// One fixed-volume label. Construction accepts exactly `[A-Za-z]:` and
/// canonicalizes the letter to uppercase before any Win32 path is built.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VolumeLabel([u8; 2]);

impl VolumeLabel {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let bytes = value.as_bytes();
        (bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
            .then(|| Self([bytes[0].to_ascii_uppercase(), b':']))
    }

    pub(crate) fn as_str(&self) -> &str {
        // The private constructor fixes both bytes to ASCII.
        std::str::from_utf8(&self.0).expect("VolumeLabel invariant is ASCII")
    }

    pub(crate) fn raw_path(self) -> String {
        format!(r"\\.\{self}")
    }

    pub(crate) fn root_path(self) -> String {
        format!("{self}\\")
    }
}

impl fmt::Display for VolumeLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::VolumeLabel;

    #[test]
    fn accepts_only_a_drive_letter_and_canonicalizes_it() {
        let label = VolumeLabel::parse("c:").expect("valid drive label");
        assert_eq!(label.as_str(), "C:");
        assert_eq!(label.raw_path(), r"\\.\C:");
        assert_eq!(label.root_path(), r"C:\");

        for invalid in [
            "", "C", "C:\\", r"\\.\C:", "CC:", "1:", "C:/", " C:", "C:\0",
        ] {
            assert!(VolumeLabel::parse(invalid).is_none(), "{invalid:?}");
        }
    }
}
