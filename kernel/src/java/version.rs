//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Java runtime version registry.
//!
//! Baseline is Java 8 (class major 52). Higher releases are recognized
//! so the kernel can report `UnsupportedClassVersionError` with a clear
//! hint instead of failing obscurely. Each new supported release only
//! needs `SUPPORTED_MAX_MAJOR` raised plus interpreter/natives coverage.

/// In-kernel JVM version (mfk-jvm, not the Java language version).
pub const MFK_JVM_VERSION: &str = "0.1.0";

/// Java language level this JVM can execute today.
pub const COMPAT_RELEASE: u32 = 8;
/// Class file major version for `COMPAT_RELEASE`.
pub const COMPAT_MAJOR: u16 = 52;

/// Highest class major the interpreter accepts. Bump as support lands.
pub const SUPPORTED_MAX_MAJOR: u16 = 52;
/// Lowest class major we bother accepting (Java 5 era, major 49).
pub const SUPPORTED_MIN_MAJOR: u16 = 45;

/// One row of the release <-> class-major mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JavaRelease {
    pub release: u32,
    pub major: u16,
    pub lts: bool,
}

/// Known releases. Keep sorted by release. Extend to 22..25 as needed;
/// majors follow the linear rule major = release + 44 (8->52, 11->55, ...).
pub const RELEASES: &[JavaRelease] = &[
    JavaRelease {
        release: 8,
        major: 52,
        lts: true,
    },
    JavaRelease {
        release: 11,
        major: 55,
        lts: true,
    },
    JavaRelease {
        release: 17,
        major: 61,
        lts: true,
    },
    JavaRelease {
        release: 21,
        major: 65,
        lts: true,
    },
];

/// Class major for a Java release, if known.
pub fn major_for_release(release: u32) -> Option<u16> {
    for r in RELEASES {
        if r.release == release {
            return Some(r.major);
        }
    }
    // Linear rule covers releases not explicitly listed (e.g. 22..25).
    if (8..=30).contains(&release) {
        return Some((release + 44) as u16);
    }
    None
}

/// Java release for a class major, if mappable.
pub fn release_for_major(major: u16) -> Option<u32> {
    for r in RELEASES {
        if r.major == major {
            return Some(r.release);
        }
    }
    // Inverse of the linear rule for the modern range.
    if (49..=80).contains(&major) {
        let rel = major as u32 - 44;
        if (8..=36).contains(&rel) {
            return Some(rel);
        }
    }
    None
}

/// Whether the in-kernel JVM can execute this class major yet.
pub fn is_supported_major(major: u16) -> bool {
    major >= SUPPORTED_MIN_MAJOR && major <= SUPPORTED_MAX_MAJOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_major_mapping() {
        assert_eq!(major_for_release(8), Some(52));
        assert_eq!(major_for_release(11), Some(55));
        assert_eq!(major_for_release(17), Some(61));
        assert_eq!(major_for_release(21), Some(65));
        assert_eq!(release_for_major(52), Some(8));
        assert_eq!(release_for_major(55), Some(11));
        assert_eq!(release_for_major(61), Some(17));
    }

    #[test]
    fn baseline_support() {
        assert!(is_supported_major(52));
        assert!(!is_supported_major(55));
        assert!(!is_supported_major(61));
    }
}
