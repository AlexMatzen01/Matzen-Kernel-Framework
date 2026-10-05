//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Archive support: `tar` (`.tar` with every common compression
//! wrapper: `.tar.gz`, `.tar.bz2`, `.tar.xz`, `.tar.lz4`, `.tar.zst`,
//! `.tar.lz`, `.tar.lzma`, `.tar.Z`), `zip`, `7z`, and the custom
//! `.mfk` container.
//!
//! Layout:
//! - [`crc32`]: shared IEEE CRC-32 for gzip + zip + mfk.
//! - [`tar`]: ustar reader/writer (GNU long names + PAX `path`/`size`).
//! - [`gzip`]: RFC 1952 framing around `miniz_oxide` raw DEFLATE.
//! - [`xz`]: `.xz` container via `lzma-rust2` (`no_std` mode).
//! - [`bz2`]: `bz2` via `libbz2-rs-sys` (`no_std`, pure Rust).
//! - [`lz4`]: LZ4 *frame* container via `lz4_flex` block codec.
//! - [`zst`]: Zstandard via `ruzstd`.
//! - [`lzip`]: `.lz` via `LzipReader`/`LzipWriter` + legacy `.lzma`.
//! - [`z`]: legacy `compress` (`.Z`) decoder (read-only).
//! - [`zip`]: PKWARE container, Stored + Deflated via `miniz_oxide`.
//! - [`sevenz`]: 7z container, Stored/LZMA/LZMA2, create+extract.
//! - [`mfk`]: custom `.mfk` container, per-entry deflate + CRC32.
//!
//! Format detection is by magic bytes first, file extension second, so
//! misnamed files still work and hostile files fail with clear errors.
//! All decompression entry points take an output `limit` — the kernel heap
//! is small (see `crate::allocator::HEAP_SIZE`), so oversized archives are
//! rejected instead of OOM-panicking.

pub mod bz2;
pub mod crc32;
pub mod cli;
pub mod gzip;
pub mod tar;
pub mod xz;
pub mod zip;
pub mod lz4;
pub mod zst;
pub mod lzip;
pub mod z;
pub mod sevenz;
pub mod mfk;

#[cfg(test)]
mod tests;

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
    TarBzip2,
    TarXz,
    TarLz4,
    TarZst,
    TarLzip,
    TarLzma,
    TarZ,
    Zip,
    SevenZ,
    Mfk,
}

fn ends_with_any(name: &str, suffixes: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    suffixes.iter().any(|s| lower.ends_with(s))
}

/// `.Z` is the one extension where case carries meaning: `.z` is a
/// different (unsupported) format, and the legacy `compress` suffix is
/// upper-case, so it cannot go through the lower-casing comparison.
fn ends_with_upper(name: &str, suffix: &str) -> bool {
    name.ends_with(suffix)
}

/// Detect the archive flavor: magic bytes win, extension breaks ties.
/// For tar, every wrapper is probed by magic before falling back to the
/// extension; an unrecognized stream falls through to [`Kind::Tar`]
/// (the parser validates checksums and reports garbage clearly).
pub fn detect(data: &[u8], name_hint: &str) -> Result<Kind, &'static str> {
    if gzip::is_gzip(data) {
        return Ok(Kind::TarGzip);
    }
    if bz2::is_bz2(data) {
        return Ok(Kind::TarBzip2);
    }
    if xz::is_xz(data) {
        return Ok(Kind::TarXz);
    }
    if lzip::is_lzip(data) {
        return Ok(Kind::TarLzip);
    }
    if lz4::is_lz4(data) {
        return Ok(Kind::TarLz4);
    }
    if zst::is_zst(data) {
        return Ok(Kind::TarZst);
    }
    if z::is_zcompress(data) {
        return Ok(Kind::TarZ);
    }
    if zip::is_zip(data) {
        return Ok(Kind::Zip);
    }
    if sevenz::is_7z(data) {
        return Ok(Kind::SevenZ);
    }
    if mfk::is_mfk(data) {
        return Ok(Kind::Mfk);
    }
    // Magic didn't match; fall back to extensions.
    //
    if ends_with_any(name_hint, &[".tar.gz", ".tgz"]) {
        return Ok(Kind::TarGzip);
    }
    if ends_with_any(name_hint, &[".tar.bz2", ".tbz2", ".tbz"]) {
        return Ok(Kind::TarBzip2);
    }
    if ends_with_any(name_hint, &[".tar.xz", ".txz"]) {
        return Ok(Kind::TarXz);
    }
    if ends_with_any(name_hint, &[".tar.lz4", ".tlz4"]) {
        return Ok(Kind::TarLz4);
    }
    if ends_with_any(name_hint, &[".tar.zst", ".tzst"]) {
        return Ok(Kind::TarZst);
    }
    if ends_with_any(name_hint, &[".tar.lz"]) {
        return Ok(Kind::TarLzip);
    }
    if ends_with_any(name_hint, &[".tar.lzma"]) {
        return Ok(Kind::TarLzma);
    }
    if ends_with_upper(name_hint, ".tar.Z") || ends_with_any(name_hint, &[".tar.z"]) {
        return Ok(Kind::TarZ);
    }
    if ends_with_any(name_hint, &[".zip"]) {
        return Ok(Kind::Zip);
    }
    if ends_with_any(name_hint, &[".7z"]) {
        return Ok(Kind::SevenZ);
    }
    if ends_with_any(name_hint, &[".mfk"]) {
        return Ok(Kind::Mfk);
    }
    if ends_with_any(name_hint, &[".gz"]) {
        return Ok(Kind::TarGzip);
    }
    if ends_with_any(name_hint, &[".bz2"]) {
        return Ok(Kind::TarBzip2);
    }
    if ends_with_any(name_hint, &[".xz"]) {
        return Ok(Kind::TarXz);
    }
    if ends_with_any(name_hint, &[".lz4"]) {
        return Ok(Kind::TarLz4);
    }
    if ends_with_any(name_hint, &[".zst"]) {
        return Ok(Kind::TarZst);
    }
    if ends_with_any(name_hint, &[".lz"]) {
        return Ok(Kind::TarLzip);
    }
    if ends_with_any(name_hint, &[".lzma"]) {
        return Ok(Kind::TarLzma);
    }
    if ends_with_upper(name_hint, ".Z") {
        return Ok(Kind::TarZ);
    }
    Ok(Kind::Tar)
}

/// Resolve a tar container to its raw tar body: borrowed for plain
/// `.tar`, freshly decompressed (owned) for all wrappers.
/// A `.zip`/`.7z`/`.mfk` input is rejected with a pointer to the
/// matching dedicated command.
/// Feed the result to [`tar::read_entries`]; keep it alive while using
/// the entries (payloads borrow it — peak heap stays near one copy).
pub fn tar_body<'a>(data: &'a [u8], name_hint: &str) -> Result<Cow<'a, [u8]>, &'static str> {
    match detect(data, name_hint)? {
        Kind::Tar => Ok(Cow::Borrowed(data)),
        Kind::TarGzip => gzip::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::TarBzip2 => bz2::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::TarXz => xz::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::TarLz4 => lz4::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::TarZst => zst::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::TarLzip => lzip::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::TarLzma => lzip::lzma1_decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::TarZ => z::decompress(data, MAX_DECOMPRESSED_BYTES).map(Cow::Owned),
        Kind::Zip => Err("tar: this is a zip archive (use 'unzip')"),
        Kind::SevenZ => Err("tar: this is a 7z archive (use '7z')"),
        Kind::Mfk => Err("tar: this is an mfk archive (use 'mfk')"),
    }
}

/// Compression applied to a freshly built tarball.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarCompress {
    /// No compression (plain `.tar`).
    None,
    /// gzip (`.tar.gz`, `-z`).
    Gzip,
    /// xz (`.tar.xz`, `-J`).
    Xz,
    /// LZ4 frame (`.tar.lz4`).
    Lz4,
    /// Zstandard (`.tar.zst`).
    Zst,
    /// lzip (`.tar.lz`).
    Lzip,
    /// Legacy headered LZMA1 (`.tar.lzma`).
    Lzma,
}

/// Map an output name to the wrapper its extension implies.
pub fn tar_compression_for(name_hint: &str) -> TarCompress {
    if ends_with_any(name_hint, &[".tar.gz", ".tgz"]) {
        TarCompress::Gzip
    } else if ends_with_any(name_hint, &[".tar.xz", ".txz"]) {
        TarCompress::Xz
    } else if ends_with_any(name_hint, &[".tar.lz4", ".tlz4"]) {
        TarCompress::Lz4
    } else if ends_with_any(name_hint, &[".tar.zst", ".tzst"]) {
        TarCompress::Zst
    } else if ends_with_any(name_hint, &[".tar.lz"]) {
        TarCompress::Lzip
    } else if ends_with_any(name_hint, &[".tar.lzma"]) {
        TarCompress::Lzma
    } else if ends_with_any(name_hint, &[".tar.bz2", ".tbz2", ".tbz"]) {
        // Decode-only in MFK; keep the error where it is raised.
        TarCompress::None
    } else if ends_with_upper(name_hint, ".tar.Z") {
        TarCompress::None
    } else {
        TarCompress::None
    }
}

/// Build a tarball, compressing by output extension:
/// `.tar.gz`/`.tgz` → gzip, `.tar.xz`/`.txz` → xz, `.tar.lz4` → lz4,
/// `.tar.zst` → zst, `.tar.lz` → lzip, `.tar.lzma` → lzma1, anything
/// else → plain. `.tar.bz2` / `.tar.Z` return an error (decode-only
/// in MFK; compress on the host and feed it in).
///
/// `forced` (from `tar -z` / `tar -J`) overrides the extension; pass `None`
/// to let the output name decide.
pub fn build_tar_auto(
    entries: &[tar::BuildEntry<'_>],
    name_hint: &str,
    forced: Option<TarCompress>,
) -> Result<Vec<u8>, &'static str> {
    let raw = tar::build(entries)?;
    // The two wrappers we can only read still report their refusal, even
    // when the caller forced some other compressor.
    if ends_with_any(name_hint, &[".tar.bz2", ".tbz2", ".tbz"]) {
        return Err("tar: .tar.bz2 creation is not supported locally (host-compress the .tar body)");
    }
    if ends_with_upper(name_hint, ".tar.Z") {
        return Err("tar: .tar.Z creation is not supported locally");
    }
    let compress = forced.unwrap_or_else(|| tar_compression_for(name_hint));
    match compress {
        TarCompress::None => Ok(raw),
        TarCompress::Gzip => Ok(gzip::compress(&raw)),
        TarCompress::Xz => xz::compress(&raw),
        TarCompress::Lz4 => lz4::compress(&raw),
        TarCompress::Zst => zst::compress(&raw),
        TarCompress::Lzip => lzip::compress(&raw),
        TarCompress::Lzma => lzip::lzma1_compress(&raw),
    }
}
