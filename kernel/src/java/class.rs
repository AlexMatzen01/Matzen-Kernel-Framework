//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Minimal Java `.class` file header parser (`no_std` + `alloc` compatible).
//!
//! Layout (big-endian):
//!   u4 magic = 0xCAFEBABE
//!   u2 minor_version
//!   u2 major_version
//! Full constant-pool/method parsing lands with the Phase 1 interpreter;
//! this header check is enough for detection, `appinfo`, and version gating.

use super::version;

/// Java class file magic, big-endian bytes `CA FE BA BE`.
pub const JAVA_CLASS_MAGIC: u32 = 0xCAFE_BABE;
/// Raw magic bytes as they appear in the file.
pub const JAVA_CLASS_MAGIC_BYTES: [u8; 4] = [0xCA, 0xFE, 0xBA, 0xBE];

/// Parsed class header: only minor/major so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassInfo {
    pub minor: u16,
    pub major: u16,
}

impl ClassInfo {
    /// Best-effort Java release for this class (e.g. 52 -> 8).
    pub fn release(&self) -> Option<u32> {
        version::release_for_major(self.major)
    }

    /// Whether the current in-kernel JVM can execute it.
    pub fn is_supported(&self) -> bool {
        version::is_supported_major(self.major)
    }
}

/// Quick magic check without parsing the version.
pub fn is_class_file(data: &[u8]) -> bool {
    data.len() >= 4 && data[0..4] == JAVA_CLASS_MAGIC_BYTES
}

/// Parse and validate the 8-byte class header.
///
/// Errors are `&'static str` to match the existing `app::run` style.
pub fn parse_header(data: &[u8]) -> Result<ClassInfo, &'static str> {
    if data.len() < 8 {
        return Err("Truncated class file (need 8-byte header)");
    }
    if data[0..4] != JAVA_CLASS_MAGIC_BYTES {
        return Err("Not a Java class (bad CAFEBABE magic)");
    }
    let minor = u16::from_be_bytes([data[4], data[5]]);
    let major = u16::from_be_bytes([data[6], data[7]]);
    if major < version::SUPPORTED_MIN_MAJOR {
        return Err("Unsupported class file version (too old)");
    }
    Ok(ClassInfo { minor, major })
}

/// Encode an 8-byte test header (used by unit tests and docs).
#[cfg(test)]
pub fn encode_test_header(minor: u16, major: u16) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0..4].copy_from_slice(&JAVA_CLASS_MAGIC_BYTES);
    out[4..6].copy_from_slice(&minor.to_be_bytes());
    out[6..8].copy_from_slice(&major.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_java8_header() {
        let hdr = encode_test_header(0, 52);
        let info = parse_header(&hdr).unwrap();
        assert_eq!(info.major, 52);
        assert_eq!(info.release(), Some(8));
        assert!(info.is_supported());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut hdr = encode_test_header(0, 52);
        hdr[0] = 0x00;
        assert!(parse_header(&hdr).is_err());
        assert!(!is_class_file(&hdr));
    }

    #[test]
    fn rejects_truncated() {
        assert!(parse_header(&[0xCA, 0xFE]).is_err());
    }

    #[test]
    fn flags_newer_release_as_unsupported() {
        let hdr = encode_test_header(0, 61);
        let info = parse_header(&hdr).unwrap();
        assert_eq!(info.release(), Some(17));
        assert!(!info.is_supported());
    }
}
