//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Archive support: `tar` (`.tar`, `.tar.gz`/`.tgz`, `.tar.xz`/`.txz`)
//! and `zip` (`.zip`, Stored + Deflated).
//!
//! Layout:
//! - [`crc32`]: shared IEEE CRC-32 for gzip + zip.
//! - [`tar`]: ustar reader/writer (GNU long names + PAX `path`/`size`).
//! - [`gzip`]: RFC 1952 framing around `miniz_oxide` raw DEFLATE.
//! - [`xz`]: `.xz` container via `lzma-rust2` (`no_std` mode).
//! - [`zip`]: PKWARE container, Stored + Deflated via `miniz_oxide`.
//!
//! Format detection is by magic bytes first, file extension second, so
//! misnamed files still work and hostile files fail with clear errors.
//! All decompression entry points take an output `limit` — the kernel heap
//! is small (see `crate::allocator::HEAP_SIZE`), so oversized archives are
//! rejected instead of OOM-panicking.

pub mod crc32;
pub mod cli;
pub mod gzip;
pub mod tar;
pub mod xz;
pub mod zip;

use alloc::borrow::Cow;
use alloc::vec::Vec;

/// Upper bound for any single decompressed payload (tar body, zip total).
/// 4 MiB keeps peak transient heap (source archive + tar body + member
/// names; payloads borrow instead of copying) well under the 16 MiB
/// kernel heap alongside FS caches and the shell.
pub const MAX_DECOMPRESSED_BYTES: usize = 4 * 1024 * 1024;

/// Archive flavor selected by [`detect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tar,
    TarGzip,
    TarXz,
    Zip,
}

fn ends_with_any(name: &str, suffixes: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    suffixes.iter().any(|s| lower.ends_with(s))
}

/// Detect the archive flavor: magic bytes win, extension breaks ties.
/// Extensionless tarballs fall through to [`Kind::Tar`] (the parser
/// validates checksums and reports garbage clearly).
pub fn detect(data: &[u8], name_hint: &str) -> Result<Kind, &'static str> {
    if gzip::is_gzip(data) {
        return Ok(Kind::TarGzip);
    }
    if xz::is_xz(data) {
        return Ok(Kind::TarXz);
    }
    if zip::is_zip(data) {
        return Ok(Kind::Zip);
    }
    if ends_with_any(name_hint, &[".tar.gz", ".tgz"]) {
        return Ok(Kind::TarGzip);
    }
    if ends_with_any(name_hint, &[".tar.xz", ".txz"]) {
        return Ok(Kind::TarXz);
    }
    if ends_with_any(name_hint, &[".zip"]) {
        return Ok(Kind::Zip);
    }
    if ends_with_any(name_hint, &[".gz"]) {
        return Ok(Kind::TarGzip);
    }
    if ends_with_any(name_hint, &[".xz"]) {
        return Ok(Kind::TarXz);
    }
    Ok(Kind::Tar)
}

/// Resolve a tar container to its raw tar body: borrowed for plain
/// `.tar`, freshly decompressed (owned) for `.tar.gz` / `.tar.xz`.
/// A `.zip` input is rejected with a pointer to `unzip`.
/// Feed the result to [`tar::read_entries`]; keep it alive while using
/// the entries (payloads borrow it — peak heap stays near one copy).
pub fn tar_body<'a>(data: &'a [u8], name_hint: &str) -> Result<Cow<'a, [u8]>, &'static str> {
    match detect(data, name_hint)? {
        Kind::Tar => Ok(Cow::Borrowed(data)),
        Kind::TarGzip => gzip::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::TarXz => xz::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::Zip => Err("tar: this is a zip archive (use 'unzip')"),
    }
}

/// Build a tarball, compressing by output extension:
/// `.tar.gz`/`.tgz` → gzip, `.tar.xz`/`.txz` → xz, anything else → plain.
pub fn build_tar_auto(
    entries: &[tar::BuildEntry<'_>],
    name_hint: &str,
) -> Result<Vec<u8>, &'static str> {
    let raw = tar::build(entries)?;
    if ends_with_any(name_hint, &[".tar.gz", ".tgz"]) {
        Ok(gzip::compress(&raw))
    } else if ends_with_any(name_hint, &[".tar.xz", ".txz"]) {
        xz::compress(&raw)
    } else {
        Ok(raw)
    }
}
