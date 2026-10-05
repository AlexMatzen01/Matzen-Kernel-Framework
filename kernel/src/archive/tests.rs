//! Host-side tests for the archive module (`cargo test -p mfk-kernel`).
//! These round-trip every container and cross-check against the reference
//! crates where possible (`sevenz-rust2`, `bzip2`, `ruzstd`, `flate2`,
//! `lz4_flex`, `tar`).

use super::*;
use crate::archive::cli::*;
use crate::archive::mfk;
use crate::archive::tar::{self, BuildEntry};
use crate::archive::zip;

/// Build a one-member ustar archive by hand, so member names the
/// reference builder refuses (traversal, absolute) can still be tested.
fn raw_tar_member(name: &str, data: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    // Octal fields, NUL terminated: mode, uid, gid, size, mtime.
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    let size = format!("{:011o}\0", data.len());
    header[124..136].copy_from_slice(size.as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    // The checksum is the header sum with the checksum field read as spaces.
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0'; // regular file
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum: u32 = header.iter().map(|b| *b as u32).sum();
    let octal = format!("{:06o}\0 ", sum);
    header[148..156].copy_from_slice(octal.as_bytes());

    let mut out = header.to_vec();
    out.extend_from_slice(data);
    let padding = (512 - data.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, padding));
    out.extend(std::iter::repeat_n(0u8, 1024)); // end-of-archive marker
    out
}

/// Append one regular file to a reference `tar` builder. The crate is
/// reached as `::tar` because our own reader module holds the name `tar`.
fn ref_tar_append(builder: &mut ::tar::Builder<Vec<u8>>, path: &str, data: &[u8]) {
    // `Header::new_gnu` makes the reference builder emit a GNU long-name
    // (`L`) entry automatically when `path` exceeds the ustar limit.
    let mut header = ::tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(::tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, path, data).unwrap();
}

/// Reference bzip2 file, produced by the `bzip2` crate (libbz2-rs backend).
fn reference_bz2(payload: &[u8]) -> Vec<u8> {
    let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(6));
    std::io::Write::write_all(&mut enc, payload).unwrap();
    enc.finish().unwrap()
}

#[test]
fn zip_roundtrip() {
    // Stored and deflated members, plus a directory entry, must all survive
    // a write/list/extract cycle with their CRCs intact.
    let a = &[b'A'; 4096][..];
    let b = b"second file contents";
    let entries = [
        zip::BuildEntry { name: "file.txt", data: Some(a) },
        zip::BuildEntry { name: "examples/", data: None },
        zip::BuildEntry { name: "examples/file2.txt", data: Some(b) },
    ];
    let archive = zip::build(&entries[..], zip::Pack::Deflated).unwrap();
    let listed = zip::list(&archive).unwrap();
    let names = listed.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert_eq!(names, vec!["file.txt", "examples", "examples/file2.txt"], "listed names");
    assert!(listed[1].is_dir, "directory flagged");
    assert_eq!(listed[0].uncomp_size, a.len() as u64, "listed uncompressed size");
    assert_eq!(listed[0].method, zip::METHOD_DEFLATED, "deflate recorded in header");
    assert!(
        listed[0].comp_size < listed[0].uncomp_size,
        "deflate actually shrinks a repetitive payload"
    );
    let out = zip::extract(&archive, super::MAX_DECOMPRESSED_BYTES).unwrap();
    let names = out.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert_eq!(names, vec!["file.txt", "examples", "examples/file2.txt"], "extracted names");
    assert_eq!(&out[0].data[..], a, "first payload");
    assert_eq!(&out[2].data[..], b, "nested payload");
    assert!(out[1].is_dir, "directory member");

    // A tiny payload falls back to stored, because deflate would expand it.
    let tiny = zip::build(
        &[zip::BuildEntry { name: "t.txt", data: Some(b"hi") }],
        zip::Pack::Deflated,
    )
    .unwrap();
    assert_eq!(
        zip::list(&tiny).unwrap()[0].method,
        zip::METHOD_STORED,
        "tiny payload falls back to stored"
    );

    // Stored mode round-trips too.
    let stored = zip::build(&entries[..], zip::Pack::Stored).unwrap();
    let out = zip::extract(&stored, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(&out[0].data[..], a, "stored first payload");
    assert_eq!(&out[2].data[..], b, "stored nested payload");
    // A corrupted payload is caught by the CRC, not written out.
    let mut corrupt = stored.clone();
    let victim = corrupt.len() - 20;
    corrupt[victim] ^= 0xFF;
    assert!(
        zip::extract(&corrupt, super::MAX_DECOMPRESSED_BYTES).is_err(),
        "corrupt stored payload"
    );
    // Traversal and absolute paths are refused on write.
    for bad in ["../escape.txt", "/abs.txt"] {
        assert!(
            zip::build(
                &[zip::BuildEntry { name: bad, data: Some(a) }],
                zip::Pack::Stored
            )
            .is_err(),
            "expected {:?} to be refused",
            bad
        );
    }}

#[test]
fn tar_variants_roundtrip() {
    // One tarball body, wrapped in every compression we support. Each must
    // detect by magic, unwrap and yield identical members.
    let body = tar::build(&[
        BuildEntry { name: "alpha.txt", data: Some(b"alpha") },
        BuildEntry { name: "sub/beta.txt", data: Some(b"beta") },
    ])
    .unwrap();
    let cases: &[(&str, crate::archive::Kind)] = &[
        ("a.tar.gz", crate::archive::Kind::TarGzip),
        ("a.tgz", crate::archive::Kind::TarGzip),
        ("a.tar.bz2", crate::archive::Kind::TarBzip2),
        ("a.tbz2", crate::archive::Kind::TarBzip2),
        ("a.tar.xz", crate::archive::Kind::TarXz),
        ("a.tar.lz4", crate::archive::Kind::TarLz4),
        ("a.tar.zst", crate::archive::Kind::TarZst),
        ("a.tar.lz", crate::archive::Kind::TarLzip),
        ("a.tar.lzma", crate::archive::Kind::TarLzma),
        ("a", crate::archive::Kind::Tar),
    ];
    for (name, expected_kind) in cases {
        let bytes = if *expected_kind == crate::archive::Kind::TarBzip2 {
            // Creation is decode-only in MFK, so use the reference encoder.
            reference_bz2(&body)
        } else {
            build_tar_auto(
                &[
                    BuildEntry { name: "alpha.txt", data: Some(b"alpha") },
                    BuildEntry { name: "sub/beta.txt", data: Some(b"beta") },
                ],
                name,
                None,
            )
            .unwrap_or_else(|e| panic!("build_tar_auto({}) failed: {}", name, e))
        };
        assert_eq!(detect(&bytes, name).unwrap(), *expected_kind, "kind for {}", name);
        let raw = tar_body(&bytes, name).unwrap();
        let entries = tar::read_entries(&raw, super::MAX_DECOMPRESSED_BYTES).unwrap();
        assert_eq!(entries.len(), 2, "member count in {}", name);
        assert_eq!(entries[0].name, "alpha.txt", "first member in {}", name);
        assert_eq!(&entries[0].data[..], b"alpha", "first payload in {}", name);
        assert_eq!(entries[1].name, "sub/beta.txt", "second member in {}", name);
        assert_eq!(&entries[1].data[..], b"beta", "second payload in {}", name);
    }

    // Creation is refused (not silently wrong) for the decode-only wrappers.
    let entries = [BuildEntry { name: "a", data: Some(b"b") }];
    assert!(build_tar_auto(&entries, "x.tar.bz2", None).is_err(), "bz2 create refused");
    assert!(build_tar_auto(&entries, "x.tbz2", None).is_err(), "tbz2 create refused");
    assert!(build_tar_auto(&entries, "x.tar.Z", None).is_err(), "Z create refused");
    // The extension aliases must all be recognised on read, not just the
    // canonical spellings used above.
    // Bare wrapper extensions (no `.tar`) are recognised too, since tarballs
    // get renamed in the wild.
    for (name, expected) in [
        ("y.gz", crate::archive::Kind::TarGzip),
        ("y.bz2", crate::archive::Kind::TarBzip2),
        ("y.xz", crate::archive::Kind::TarXz),
        ("y.lz4", crate::archive::Kind::TarLz4),
        ("y.zst", crate::archive::Kind::TarZst),
        ("y.lz", crate::archive::Kind::TarLzip),
        ("y.lzma", crate::archive::Kind::TarLzma),
        ("y.Z", crate::archive::Kind::TarZ),
        ("y.zip", crate::archive::Kind::Zip),
        ("y.7z", crate::archive::Kind::SevenZ),
        ("y.mfk", crate::archive::Kind::Mfk),
        ("y.bin", crate::archive::Kind::Tar),
    ] {
        assert_eq!(
            detect(b"not really an archive", name).unwrap(),
            expected,
            "extension fallback for {}",
            name
        );
    }
    // `.Z` is case-significant (legacy `compress`); a lower-case `.z` is
    // some other format, so it must not be mistaken for one.
    assert_eq!(
        detect(b"not really an archive", "y.z").unwrap(),
        crate::archive::Kind::Tar,
        "lower-case .z is not legacy compress"
    );
    // The supported creation wrappers all produce a detectable archive.
    for name in [
        "x.tar", "x.tar.gz", "x.tgz", "x.tar.xz", "x.txz", "x.tar.lz4", "x.tar.zst", "x.tar.lz",
        "x.tar.lzma",
    ] {
        let bytes = build_tar_auto(&entries, name, None).unwrap();
        let body = tar_body(&bytes, name)
            .unwrap_or_else(|e| panic!("{} did not decode: {}", name, e));
        let read = tar::read_entries(&body, super::MAX_DECOMPRESSED_BYTES).unwrap();
        assert_eq!(read.len(), 1, "member count in {}", name);
        assert_eq!(&read[0].data[..], b"b", "payload in {}", name);
    }

    // gzip must be readable by the reference decoder (RFC 1952 trailer).
    let bytes = build_tar_auto(&entries, "x.tar.gz", None).unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut out).unwrap();
    let raw = tar::read_entries(&out, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(raw[0].name, "a", "reference decoder reads our gzip");

    // A flipped gzip CRC byte is caught rather than silently accepted.
    let mut corrupt = bytes.clone();
    let n = corrupt.len();
    corrupt[n - 8] ^= 0xFF;
    assert!(tar_body(&corrupt, "x.tar.gz").is_err(), "gzip CRC mismatch");
}

#[test]
fn tar_reader_accepts_reference_tarballs() {
    // A tarball written by the reference `tar` crate must list and extract
    // here. The crate is reached as `::tar` because our own reader module
    // occupies the name `tar` in this file's scope.
    let data: &[u8] = b"reference tar payload";
    let mut builder = ::tar::Builder::new(Vec::new());
    ref_tar_append(&mut builder, "ref.txt", data);
    let bytes = builder.into_inner().unwrap();

    assert_eq!(detect(&bytes, "ref.tar").unwrap(), crate::archive::Kind::Tar);
    let raw = tar_body(&bytes, "ref.tar").unwrap();
    let entries = tar::read_entries(&raw, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(entries.len(), 1, "single member");
    assert_eq!(entries[0].name, "ref.txt", "member name");
    assert_eq!(&entries[0].data[..], data, "member payload");

    // A member name over the 100-byte ustar limit must resolve through the
    // GNU long-name (`L`) entry rather than being truncated, and the same
    // name written as a PAX extended header must work too. Each path
    // component stays within SimplFS's 55-byte limit.
    let long = "d".repeat(50);
    let long_name = format!("{}/{}/leaf.txt", long, "b".repeat(50));
    let mut builder = ::tar::Builder::new(Vec::new());
    ref_tar_append(&mut builder, &long_name, data);
    let bytes = builder.into_inner().unwrap();
    // The long name must survive a round trip through the `L` entry.
    let entries = tar::read_entries(&bytes, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(entries.len(), 1, "gnu long name");
    assert_eq!(entries[0].name, long_name, "gnu long name");
    assert_eq!(&entries[0].data[..], data, "gnu long name");

    // The same name written through a PAX extended header is honoured too.
    let mut builder = ::tar::Builder::new(Vec::new());
    let mut header = ::tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(::tar::EntryType::Regular);
    header.set_cksum();
    let size_field = data.len().to_string();
    let pax: Vec<(&str, &[u8])> = vec![
        ("path", long_name.as_bytes()),
        ("size", size_field.as_bytes()),
    ];
    builder.append_pax_extensions(pax).unwrap();
    builder.append_data(&mut header, "placeholder", data).unwrap();
    let bytes = builder.into_inner().unwrap();
    let entries = tar::read_entries(&bytes, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(entries.len(), 1, "pax long name");
    assert_eq!(entries[0].name, long_name, "pax path override");
    assert_eq!(&entries[0].data[..], data, "pax payload");

    // Directories and empty files round-trip with the right flags.
    let mut builder = ::tar::Builder::new(Vec::new());
    let mut header = ::tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o755);
    header.set_mtime(0);
    header.set_entry_type(::tar::EntryType::Directory);
    header.set_cksum();
    builder.append_data(&mut header, "d", &b""[..]).unwrap();
    ref_tar_append(&mut builder, "d/empty", b"");
    let bytes = builder.into_inner().unwrap();
    let entries = tar::read_entries(&bytes, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(entries.len(), 2, "dir + empty file");
    assert!(entries[0].is_dir, "directory flagged");
    assert_eq!(entries[0].name, "d", "directory name");
    assert!(!entries[1].is_dir, "empty file is not a directory");
    assert!(entries[1].data.is_empty(), "empty file payload");

    // A single path component longer than SimplFS can store is refused at
    // read time too, so a hostile archive cannot smuggle one in.
    let overlong = "x".repeat(200);
    assert!(
        tar::read_entries(&raw_tar_member(&overlong, data), super::MAX_DECOMPRESSED_BYTES).is_err(),
        "over-long path component"
    );

    // Traversal in a member name is refused rather than written outside.
    // The reference builder refuses to write such a name, so craft the
    // header bytes directly — that is exactly what a hostile archive does.
    for bad in ["../escape.txt", "/etc/passwd", "a/../../b.txt"] {
        assert!(
            tar::read_entries(&raw_tar_member(bad, b""), super::MAX_DECOMPRESSED_BYTES).is_err(),
            "expected {:?} to be refused",
            bad
        );
    }
    // A truncated archive is an error, not a partial listing.
    let mut builder = ::tar::Builder::new(Vec::new());
    ref_tar_append(&mut builder, "a.txt", data);
    let bytes = builder.into_inner().unwrap();
    assert!(tar::read_entries(&bytes[..600], super::MAX_DECOMPRESSED_BYTES).is_err());

    // The hand-built fixture is itself a valid ustar member, so the test
    // above really exercised the reader's own parsing.
    let hand = raw_tar_member("plain.txt", data);
    let entries = tar::read_entries(&hand, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(entries.len(), 1, "hand-built ustar member");
    assert_eq!(entries[0].name, "plain.txt", "hand-built member name");
    assert_eq!(&entries[0].data[..], data, "hand-built member payload");
}

#[test]
fn mfk_roundtrip() {
    // A reader/writer pair over mixed directory and file members.
    let archive = mfk::build(
        &[
            mfk::BuildEntry { name: "hello.txt", data: Some(b"hi there") },
            mfk::BuildEntry { name: "docs/", data: None },
            mfk::BuildEntry { name: "docs/a.txt", data: Some(b"A") },
        ],
        mfk::Pack::Deflate,
    )
    .unwrap();
    let infos = mfk::list(&archive).unwrap();
    assert_eq!(infos.len(), 3, "listed entry count");
    assert_eq!(infos[0].name, "hello.txt", "first member name");
    assert_eq!(infos[0].uncomp_size, 8, "first member uncompressed size");
    assert!(infos[1].is_dir, "directory flagged");
    assert_eq!(infos[1].uncomp_size, 0, "directory has no payload");
    let out = mfk::extract(&archive, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out.len(), 3, "extracted entry count");
    assert_eq!(&out[0].data[..], b"hi there", "first member payload");
    assert!(out[1].is_dir, "directory member");
    assert_eq!(&out[2].data[..], b"A", "nested member payload");

    // A compressible payload is deflated; a tiny one is stored verbatim,
    // because deflate would make it larger.
    let compressible = &[b'M'; 4096][..];
    let deflated = mfk::build(
        &[mfk::BuildEntry { name: "big.txt", data: Some(compressible) }],
        mfk::Pack::Deflate,
    )
    .unwrap();
    let infos = mfk::list(&deflated).unwrap();
    assert!(!infos[0].stored, "compressible payload is deflated");
    assert!(infos[0].comp_size < infos[0].uncomp_size, "deflate actually shrinks it");
    let out = mfk::extract(&deflated, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out[0].data, compressible, "deflated payload round-trips");

    let tiny = mfk::build(
        &[mfk::BuildEntry { name: "tiny.txt", data: Some(b"hi") }],
        mfk::Pack::Deflate,
    )
    .unwrap();
    let infos = mfk::list(&tiny).unwrap();
    assert!(infos[0].stored, "tiny payload falls back to stored");
    assert_eq!(infos[0].comp_size, infos[0].uncomp_size, "stored size is payload size");

    // Stored mode keeps the payload verbatim.
    let stored = mfk::build(
        &[mfk::BuildEntry { name: "s.bin", data: Some(b"\x00\x01\x02\x03") }],
        mfk::Pack::Stored,
    )
    .unwrap();
    let infos = mfk::list(&stored).unwrap();
    assert!(infos[0].stored, "stored mode recorded");
    assert_eq!(infos[0].comp_size, infos[0].uncomp_size, "stored size is the payload size");
    let out = mfk::extract(&stored, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(&out[0].data[..], b"\x00\x01\x02\x03", "stored payload");
}

#[test]
fn sevenz_roundtrip() {
    let archive = crate::archive::sevenz::build(&[
        BuildEntry { name: "one.txt", data: Some(b"first contents here") },
        BuildEntry { name: "sub/", data: None },
        BuildEntry { name: "sub/two.txt", data: Some(b"second contents there") },
    ])
    .unwrap();
    let infos = crate::archive::sevenz::list(&archive).unwrap();
    assert_eq!(infos.len(), 3, "listed entry count");
    assert!(!infos[0].is_dir, "file is not a directory");
    assert_eq!(infos[0].name, "one.txt", "first member name");
    assert!(infos[1].is_dir, "directory flagged");
    // Payload sizes are reported from the header without decoding.
    assert_eq!(infos[0].size, 19, "first member size");
    assert_eq!(infos[2].size, 21, "third member size");
    let out = crate::archive::sevenz::extract(&archive, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out.len(), 3, "extracted entry count");
    assert_eq!(out[0].0, "one.txt", "first member name");
    assert_eq!(&out[0].2[..], b"first contents here", "first member payload");
    assert!(out[1].1, "directory member");
    assert!(out[1].2.is_empty(), "directory has no payload");
    assert_eq!(out[2].0, "sub/two.txt", "third member name");
    assert_eq!(&out[2].2[..], b"second contents there", "third member payload");

    // A corruption anywhere in the packed streams is caught (CRC per member).
    let mut corrupt = archive.clone();
    let victim = corrupt.len() - 8;
    corrupt[victim] ^= 0x5A;
    assert!(
        crate::archive::sevenz::extract(&corrupt, super::MAX_DECOMPRESSED_BYTES).is_err(),
        "corrupt packed stream"
    );

    // Round-trip must be host-compatible: sevenz-rust2 must read our archive.
    let dir = std::env::temp_dir().join("mfk7z_out");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let arch = std::env::temp_dir().join("mfk-test.7z");
    std::fs::write(&arch, &archive).unwrap();
    sevenz_rust2::decompress_file(&arch, &dir).unwrap();
    let one = std::fs::read(dir.join("one.txt")).unwrap();
    assert_eq!(one, b"first contents here", "host extracted first member");
    assert_eq!(
        std::fs::read(dir.join("sub/two.txt")).unwrap(),
        b"second contents there",
        "host extracted nested member"
    );
}

#[test]
fn sevenz_read_host_archive() {
    // The reference archiver writes an LZMA2-compressed header and a solid
    // folder, i.e. the opposite layout to ours — so this exercises the
    // encoded-header path and multi-sub-stream splitting.
    let src_dir = std::env::temp_dir().join("mfk7z_src");
    let _ = std::fs::remove_dir_all(&src_dir);
    let _ = std::fs::create_dir_all(&src_dir);
    std::fs::write(src_dir.join("hello.txt"), b"hello from host 7z").unwrap();
    std::fs::write(src_dir.join("world.txt"), b"world from host 7z").unwrap();
    std::fs::write(src_dir.join("big.bin"), &[0x5Au8; 300 * 1024]).unwrap();
    let arch = std::env::temp_dir().join("mfk-host.7z");
    sevenz_rust2::compress_to_path(&src_dir, &arch).unwrap();
    let data = std::fs::read(&arch).unwrap();
    let infos = crate::archive::sevenz::list(&data).unwrap();
    // The reference archiver also records the containing directory.
    assert!(infos.len() >= 3, "expected at least 3 entries, got {:?}", infos.len());
    assert!(infos.iter().any(|e| e.name.ends_with("hello.txt")), "hello listed");
    assert!(infos.iter().any(|e| e.name.ends_with("big.bin")), "big listed");
    let out = crate::archive::sevenz::extract(&data, super::MAX_DECOMPRESSED_BYTES).unwrap();
    let files: Vec<&Vec<u8>> = out.iter().filter(|e| !e.1).map(|e| &e.2).collect();
    assert_eq!(files.len(), 3, "expected 3 file members, got {:?}", files.len());
    assert!(files.iter().any(|d| d.as_slice() == b"hello from host 7z"), "hello payload");
    assert!(files.iter().any(|d| d.as_slice() == b"world from host 7z"), "world payload");
    let big = files
        .iter()
        .find(|d| d.len() == 300 * 1024)
        .expect("big member present");
    assert!(big.iter().all(|b| *b == 0x5A), "big payload bytes");
    // Directory members are reported as directories, not empty files.
    assert!(out.iter().any(|e| e.1), "directory member present");
}

#[test]
fn sevenz_supports_every_folder_method() {
    // Stored, LZMA1 and LZMA2 are the codecs the reader accepts, and all
    // three must both round-trip here and be readable by a host archiver.
    use crate::archive::sevenz::Method;
    let entries = [
        BuildEntry { name: "empty-dir/", data: None },
        BuildEntry { name: "small.txt", data: Some(b"a short member") },
        BuildEntry { name: "dir/big.bin", data: Some(&[0xA5u8; 200 * 1024]) },
    ];
    for method in [Method::Stored, Method::Lzma, Method::Lzma2] {
        let archive = crate::archive::sevenz::build_with(&entries, method).unwrap();
        let infos = crate::archive::sevenz::list(&archive).unwrap();
        assert_eq!(infos.len(), 3, "{:?}: entry count", method);
        assert!(infos[0].is_dir, "{:?}: directory flagged", method);
        assert_eq!(infos[1].name, "small.txt", "{:?}: first file name", method);
        let out = crate::archive::sevenz::extract(&archive, super::MAX_DECOMPRESSED_BYTES).unwrap();
        assert_eq!(out.len(), 3, "{:?}: extracted count", method);
        assert!(out[0].1, "{:?}: directory payload empty", method);
        assert!(out[0].2.is_empty(), "{:?}: directory has no payload", method);
        assert_eq!(&out[1].2[..], b"a short member", "{:?}: small payload", method);
        assert_eq!(out[2].2.len(), 200 * 1024, "{:?}: large payload length", method);
        assert!(out[2].2.iter().all(|b| *b == 0xA5), "{:?}: large payload bytes", method);
        assert_eq!(out[2].0, "dir/big.bin", "{:?}: large member name", method);

        // Host tools must accept every method we emit.
        let dir = std::env::temp_dir().join("mfk7z_methods");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let arch = std::env::temp_dir().join("mfk-methods.7z");
        std::fs::write(&arch, &archive).unwrap();
        sevenz_rust2::decompress_file(&arch, &dir)
            .unwrap_or_else(|e| panic!("sevenz-rust2 rejected {:?} archive: {:?}", method, e));
        assert_eq!(
            std::fs::read(dir.join("small.txt")).unwrap(),
            b"a short member",
            "{:?}",
            method
        );
    }
}

#[test]
fn sevenz_rejects_damaged_input() {
    // A 7z file has two CRCs (start header and next header) plus per-member
    // digests; each is a hard failure rather than a partial read.
    let archive = crate::archive::sevenz::build(&[
        BuildEntry { name: "a.txt", data: Some(b"payload bytes") },
    ])
    .unwrap();
    // A damaged start header fails its CRC.
    let mut bogus = archive.clone();
    bogus[8] ^= 0xFF;
    assert!(
        crate::archive::sevenz::extract(&bogus, super::MAX_DECOMPRESSED_BYTES).is_err(),
        "corrupt start header"
    );
    // Truncation is an error, never a partial read.
    assert!(
        crate::archive::sevenz::extract(&archive[..20], super::MAX_DECOMPRESSED_BYTES).is_err(),
        "truncated before header"
    );
    assert!(
        crate::archive::sevenz::extract(&archive[..archive.len() - 4], super::MAX_DECOMPRESSED_BYTES)
            .is_err(),
        "truncated header"
    );
    // Not a 7z at all.
    assert!(!crate::archive::sevenz::is_7z(b"not a 7z file"), "foreign data has no signature");
    assert!(
        crate::archive::sevenz::extract(b"nope", super::MAX_DECOMPRESSED_BYTES).is_err(),
        "not a 7z"
    );
}

#[test]
fn sevenz_name_traversal_is_refused() {
    // Member names that would escape the extraction directory are refused
    // at write time, for every folder method.
    for bad in ["../escape.txt", "/abs.txt", "a/../../b.txt", "a\\b.txt"] {
        assert!(
            crate::archive::sevenz::build(&[BuildEntry { name: bad, data: Some(b"x") }]).is_err(),
            "expected {:?} to be refused",
            bad
        );
    }
}

/// Every container routes member names through `tar::sanitize_path`, so a
/// hostile archive cannot name a path outside the destination. The check
/// lives in one place; this proves each writer goes through it.
#[test]
fn every_writer_refuses_path_traversal() {
    for bad in ["../escape.txt", "/abs.txt", "a/../../b.txt", "a\\b.txt"] {
        assert!(
            build_tar_auto(&[BuildEntry { name: bad, data: Some(b"x") }], "x.tar", None).is_err(),
            "tar accepted {:?}",
            bad
        );
        assert!(
            zip::build(&[zip::BuildEntry { name: bad, data: Some(b"x") }], zip::Pack::Deflated).is_err(),
            "zip accepted {:?}",
            bad
        );
        assert!(
            mfk::build(&[mfk::BuildEntry { name: bad, data: Some(b"x") }], mfk::Pack::Deflate).is_err(),
            "mfk accepted {:?}",
            bad
        );
        assert!(
            crate::archive::sevenz::build(&[BuildEntry { name: bad, data: Some(b"x") }]).is_err(),
            "7z accepted {:?}",
            bad
        );
    }
}

#[test]
fn bz2_decompress() {
    // bzip2 is decode-only in MFK, so every fixture comes from the
    // reference encoder.
    let data = reference_bz2(b"bzip2 payload");
    assert!(crate::archive::bz2::is_bz2(&data), "bz2 magic");
    let out = crate::archive::bz2::decompress(&data, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, b"bzip2 payload", "small bz2 payload");

    // Multi-megabyte payloads exercise the streaming loop, not just one shot.
    let big: Vec<u8> = (0..(3 * 1024 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let data = reference_bz2(&big);
    let out = crate::archive::bz2::decompress(&data, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out.len(), big.len(), "large bz2 payload length");
    assert_eq!(out, big, "large bz2 payload");

    // A truncated stream is an error, never a short read.
    let truncated = &data[..data.len() / 2];
    assert!(
        crate::archive::bz2::decompress(truncated, super::MAX_DECOMPRESSED_BYTES).is_err(),
        "truncated bz2"
    );
    // Garbage with the right magic fails cleanly.
    assert!(
        crate::archive::bz2::decompress(b"BZh9garbagegarbage", 1024).is_err(),
        "garbage with bz2 magic"
    );
    // The output cap is enforced.
    assert!(
        crate::archive::bz2::decompress(&reference_bz2(&big), 1024).is_err(),
        "bz2 output cap"
    );
}

#[test]
fn zst_roundtrip() {
    // Zstandard in both directions, plus the frame-concatenation rule.
    let data = crate::archive::zst::compress(b"zstd payload").unwrap();
    let out = crate::archive::zst::decompress(&data, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, b"zstd payload", "our zst frame");

    // Frames produced by the reference encoder must decode here.
    let data = ruzstd::encoding::compress_to_vec(
        &b"reference zstd payload"[..],
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    let out = crate::archive::zst::decompress(&data, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, b"reference zstd payload", "reference zst frame");

    // Multi-frame streams concatenate, per the zstd frame spec.
    let mut joined = crate::archive::zst::compress(b"first ").unwrap();
    joined.extend_from_slice(&crate::archive::zst::compress(b"second").unwrap());
    let out = crate::archive::zst::decompress(&joined, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, b"first second", "multi-frame zst");

    // Large payloads cross the streaming path.
    let big: Vec<u8> = (0..(512 * 1024u32)).map(|i| (i % 241) as u8).collect();
    let big_frame = crate::archive::zst::compress(&big).unwrap();
    let out = crate::archive::zst::decompress(&big_frame, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out.len(), big.len(), "large zst payload length");
    assert_eq!(out, big, "large zst payload");

    // Magic probes agree with the decoders.
    assert!(crate::archive::zst::is_zst(&data), "zst magic");
    assert!(crate::archive::bz2::is_bz2(b"BZh1"), "bz2 magic");
    assert!(!crate::archive::bz2::is_bz2(b"BZx1"), "bz2 magic rejected");
    assert!(crate::archive::lz4::is_lz4(&[0x04, 0x22, 0x4D, 0x18]), "lz4 magic");
    assert!(!crate::archive::lz4::is_lz4(&[0x04, 0x22, 0x4D, 0x19]), "lz4 magic rejected");
    assert!(crate::archive::gzip::is_gzip(&[0x1F, 0x8B]), "gzip magic");
    assert!(crate::archive::xz::is_xz(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]), "xz magic");
    assert!(crate::archive::z::is_zcompress(&[0x1F, 0x9D, 0x9D]), "Z magic");
    assert!(!crate::archive::z::is_zcompress(&[0x1F, 0x9E]), "Z magic rejected");

    // Bad magic and the output cap are both refused.
    assert!(!crate::archive::zst::is_zst(b"definitely not zstd"), "zst magic rejected");
    assert!(
        crate::archive::zst::decompress(b"definitely not zstd", 1024).is_err(),
        "bad zst magic"
    );
    let big_frame = crate::archive::zst::compress(&big).unwrap();
    assert!(
        crate::archive::zst::decompress(&big_frame, 1024).is_err(),
        "zst output cap"
    );
    // A frame cut in half is an error, not a short read.
    assert!(
        crate::archive::zst::decompress(
            &big_frame[..big_frame.len() / 2],
            super::MAX_DECOMPRESSED_BYTES
        )
        .is_err(),
        "truncated zst frame"
    );
}

#[test]
fn lz4_roundtrip() {
    // The frame container is parsed in-kernel on top of the no_std block
    // codec, so every header shape has to be handled: our own writer, the
    // reference encoder, and linked blocks with checksums.
    let compressed = crate::archive::lz4::compress(b"lz4 payload bytes").unwrap();
    let out = crate::archive::lz4::decompress(&compressed, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, b"lz4 payload bytes", "our frame decodes");
    // Our own frames must be readable by the reference decoder, header
    // checksum included.
    let mut decoder = lz4_flex::frame::FrameDecoder::new(&compressed[..]);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut out).unwrap();
    assert_eq!(out, b"lz4 payload bytes", "reference decoder reads our frame");
    // The header checksum must be a real XXH32-derived value, not a filler.
    assert_eq!(
        compressed[6],
        crate::archive::lz4::header_checksum(&compressed[4..6]),
        "our own header checksum"
    );
    // ...and a wrong one is rejected on decode.
    let mut bad = compressed.clone();
    bad[6] ^= 0xFF;
    assert!(
        crate::archive::lz4::decompress(&bad, super::MAX_DECOMPRESSED_BYTES).is_err(),
        "corrupt header checksum"
    );

    // Frame produced by the lz4_flex reference frame encoder must decode.
    let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
    std::io::Write::write_all(&mut encoder, b"frame payload").unwrap();
    let data = encoder.finish().unwrap();
    let out = crate::archive::lz4::decompress(&data, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, b"frame payload", "reference frame decodes");

    // Linked blocks with both checksum flavors: later blocks reference
    // earlier output through the sliding window.
    let mut payload = Vec::new();
    for i in 0..4000u32 {
        payload.extend_from_slice(format!("line {} of repeated text\n", i).as_bytes());
    }
    for mode in [
        lz4_flex::frame::BlockMode::Linked,
        lz4_flex::frame::BlockMode::Independent,
    ] {
        let mut info = lz4_flex::frame::FrameInfo::default();
        info.block_size = lz4_flex::frame::BlockSize::Max64KB;
        info.block_mode = mode;
        info.content_checksum = true;
        info.block_checksums = true;
        let mut enc =
            lz4_flex::frame::FrameEncoder::with_frame_info(info, Vec::<u8>::new());
        std::io::Write::write_all(&mut enc, &payload).unwrap();
        let data = enc.finish().unwrap();
        let out = crate::archive::lz4::decompress(&data, super::MAX_DECOMPRESSED_BYTES).unwrap();
        assert_eq!(out, payload, "frame with {:?}", mode);
    }

    // The output cap is enforced instead of truncating.
    let big = vec![3u8; 300 * 1024];
    let data = crate::archive::lz4::compress(&big).unwrap();
    assert!(
        crate::archive::lz4::decompress(&data, 1024).is_err(),
        "lz4 output cap"
    );
    // Truncation and bad magic are clean errors.
    assert!(
        crate::archive::lz4::decompress(&data[..data.len() / 2], super::MAX_DECOMPRESSED_BYTES)
            .is_err(),
        "truncated lz4 frame"
    );
    assert!(
        crate::archive::lz4::decompress(b"not lz4", 1024).is_err(),
        "bad lz4 magic"
    );
    // The magic probe agrees with the decoder.
    assert!(crate::archive::lz4::is_lz4(&data), "our frame has the magic");
    assert!(!crate::archive::lz4::is_lz4(b"not lz4"), "foreign data has no magic");
}

#[test]
fn lzip_and_lzma1_roundtrip() {
    // `.lz` (lzip container) and legacy `.lzma` (headered LZMA1) are two
    // different wrappers around the same codec; both must round-trip.
    let compressed = crate::archive::lzip::compress(b"lzip payload").unwrap();
    let out = crate::archive::lzip::decompress(&compressed, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, b"lzip payload", "lzip roundtrip");

    let compressed = crate::archive::lzip::lzma1_compress(b"lzma payload").unwrap();
    let out = crate::archive::lzip::lzma1_decompress(&compressed, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, b"lzma payload", "lzma1 roundtrip");

    // Larger payloads cross the internal 32 KiB scratch buffer.
    let big: Vec<u8> = (0..(256 * 1024u32)).map(|i| (i % 253) as u8).collect();
    let compressed = crate::archive::lzip::compress(&big).unwrap();
    let out = crate::archive::lzip::decompress(&compressed, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, big, "large lzip payload");
    let compressed = crate::archive::lzip::lzma1_compress(&big).unwrap();
    let out = crate::archive::lzip::lzma1_decompress(&compressed, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(out, big, "large lzma1 payload");

    // Bad magic is refused before any decode work.
    assert!(!crate::archive::lzip::is_lzip(b"not lzip at all"), "lzip magic rejected");
    assert!(
        crate::archive::lzip::decompress(b"not lzip at all", 1024).is_err(),
        "bad lzip magic"
    );
    assert!(
        crate::archive::lzip::lzma1_decompress(b"nope", 1024).is_err(),
        "bad lzma header"
    );
    // The output cap is enforced rather than truncating.
    let compressed = crate::archive::lzip::compress(&big).unwrap();
    assert!(
        crate::archive::lzip::decompress(&compressed, 1024).is_err(),
        "lzip output cap"
    );
    // A cut stream is an error, not a short read.
    assert!(
        crate::archive::lzip::decompress(
            &compressed[..compressed.len() / 2],
            super::MAX_DECOMPRESSED_BYTES
        )
        .is_err(),
        "truncated lzip stream"
    );
}

#[test]
fn mfk_rejects_corruption_and_traversal() {
    let archive = mfk::build(
        &[mfk::BuildEntry { name: "a.txt", data: Some(b"payload") }],
        mfk::Pack::Deflate,
    )
    .unwrap();

    // Flipping a payload byte trips the CRC check. The payload sits right
    // after the 4-byte magic, the 16-byte entry header and the name.
    let payload_at = 4 + 16 + "a.txt".len();
    let mut corrupt = archive.clone();
    corrupt[payload_at] ^= 0xFF;
    assert!(
        mfk::extract(&corrupt, super::MAX_DECOMPRESSED_BYTES).is_err(),
        "corrupt payload"
    );

    // A wrong stored CRC is rejected without decoding.
    let stored = mfk::build(
        &[mfk::BuildEntry { name: "a.txt", data: Some(b"payload") }],
        mfk::Pack::Stored,
    )
    .unwrap();
    let mut corrupt = stored.clone();
    // CRC is the 5th..=8th byte of the entry header.
    corrupt[4 + 10] ^= 0xFF;
    assert!(
        mfk::extract(&corrupt, super::MAX_DECOMPRESSED_BYTES).is_err(),
        "wrong stored CRC"
    );

    // Traversal, absolute paths, backslashes and empty names are all
    // rejected on write rather than at extraction time.
    for bad in ["../escape.txt", "/abs.txt", "a\\b.txt", "", ".."] {
        assert!(
            mfk::build(
                &[mfk::BuildEntry { name: bad, data: Some(b"x") }],
                mfk::Pack::Stored
            )
            .is_err(),
            "expected {:?} to be rejected",
            bad
        );
    }
    // A drive-prefixed name could escape the destination drive.
    assert!(
        mfk::build(
            &[mfk::BuildEntry { name: "2:/x.txt", data: Some(b"x") }],
            mfk::Pack::Stored
        )
        .is_err(),
        "drive prefix"
    );

    // A directory member may not carry a payload.
    assert!(
        mfk::build(&[mfk::BuildEntry { name: "d/", data: Some(b"x") }], mfk::Pack::Stored)
            .is_err(),
        "directory with payload"
    );

    // Truncation fails rather than yielding partial data, and a missing
    // magic is refused.
    assert!(
        mfk::extract(&archive[..archive.len() - 4], super::MAX_DECOMPRESSED_BYTES).is_err(),
        "truncated before terminator"
    );
    assert!(
        mfk::extract(&archive[..archive.len() - 1], super::MAX_DECOMPRESSED_BYTES).is_err(),
        "truncated terminator"
    );
    assert!(
        mfk::extract(b"MFK2not ours", super::MAX_DECOMPRESSED_BYTES).is_err(),
        "wrong magic"
    );
    assert!(mfk::list(b"nope").is_err(), "list rejects foreign data");
}

/// The kernel heap is 16 MiB (see `crate::allocator::HEAP_SIZE`), so every
/// decode path takes an output cap. Exceeding it must be an error, never a
/// silent truncation: a half-written file is worse than a failed command.
#[test]
fn oversized_archives_are_rejected_not_truncated() {
    // A member that decompresses past the cap is refused.
    let archive = mfk::build(
        &[mfk::BuildEntry { name: "big.bin", data: Some(&[7u8; 4096]) }],
        mfk::Pack::Deflate,
    )
    .unwrap();
    assert!(mfk::extract(&archive, 1024).is_err(), "deflate member over cap");
    // A stored member is not decompressed, but it is still capped.
    let stored = mfk::build(
        &[mfk::BuildEntry { name: "big.bin", data: Some(&[7u8; 4096]) }],
        mfk::Pack::Stored,
    )
    .unwrap();
    assert!(mfk::extract(&stored, 1024).is_err(), "stored member over cap");
    // The same cap applies across the whole archive, not per member.
    let multi = mfk::build(
        &[
            mfk::BuildEntry { name: "a.bin", data: Some(&[1u8; 700]) },
            mfk::BuildEntry { name: "b.bin", data: Some(&[2u8; 700]) },
        ],
        mfk::Pack::Stored,
    )
    .unwrap();
    assert!(mfk::extract(&multi, 1024).is_err(), "cumulative cap across members");
    assert!(
        mfk::extract(&multi, 4096)
            .unwrap_or_else(|e| panic!("cap large enough for both members: {}", e))
            .len()
            == 2,
        "both members extracted under a sufficient cap"
    );
    // Listing never buffers payloads, so it works under any cap.
    assert_eq!(mfk::list(&multi).unwrap().len(), 2, "listing is cap-independent");
    // 7z honours the same cap.
    let sevenz = crate::archive::sevenz::build(&[
        BuildEntry { name: "a.bin", data: Some(&[3u8; 8192]) },
    ])
    .unwrap();
    assert!(crate::archive::sevenz::extract(&sevenz, 1024).is_err(), "7z output cap");
    // The tar body limit is honoured too.
    let probe = tar::build(&[BuildEntry { name: "a", data: Some(b"payload") }]).unwrap();
    assert_eq!(
        tar::read_entries(&probe, 4).unwrap_err(),
        "tar: archive too large",
        "tar honours its cap"
    );
    assert!(tar::read_entries(&probe, 64).is_ok(), "cap large enough for tar");
    assert!(MAX_DECOMPRESSED_BYTES >= 8192, "default cap fits a test member");
}

#[test]
fn detect_prefers_magic() {
    // Magic wins over the extension, so misnamed archives still work.
    let data = build_tar_auto(
        &[BuildEntry { name: "a", data: Some(b"b") }],
        "a.tar.gz",
        None,
    )
    .unwrap();
    assert_eq!(detect(&data, "a.tar.gz").unwrap(), crate::archive::Kind::TarGzip, "gz by magic");
    assert!(
        matches!(detect(b"garbage", "a.bin"), Ok(crate::archive::Kind::Tar)),
        "unknown data falls through to tar"
    );
    assert_eq!(
        detect(&data, "lying.zip").unwrap(),
        crate::archive::Kind::TarGzip,
        "magic beats a misleading extension"
    );
    let zst = crate::archive::zst::compress(b"payload").unwrap();
    assert_eq!(detect(&zst, "lying.tar").unwrap(), crate::archive::Kind::TarZst, "zst by magic");
    // The custom container is recognized by its own magic.
    let mfk_archive = mfk::build(
        &[mfk::BuildEntry { name: "a", data: Some(b"b") }],
        mfk::Pack::Deflate,
    )
    .unwrap();
    assert_eq!(detect(&mfk_archive, "a.tar").unwrap(), crate::archive::Kind::Mfk, "mfk by magic");
    // ...and tar refuses to unpack the dedicated containers.
    assert!(tar_body(&mfk_archive, "a.mfk").is_err(), "tar declines mfk");
    let sevenz = crate::archive::sevenz::build(&[BuildEntry { name: "a", data: Some(b"b") }]).unwrap();
    assert_eq!(detect(&sevenz, "a.zip").unwrap(), crate::archive::Kind::SevenZ, "7z by magic");
    assert!(tar_body(&sevenz, "a.7z").is_err(), "tar declines 7z");
    // Zip is likewise declined by tar.
    let zipped = zip::build(
        &[zip::BuildEntry { name: "a", data: Some(b"b") }],
        zip::Pack::Deflated,
    )
    .unwrap();
    assert!(tar_body(&zipped, "a.zip").is_err(), "tar declines zip");
}

/// The XXH32 implementation behind the LZ4 header checksum is only
/// trustworthy if it agrees with the reference encoder, and our decoder
/// verifies the checksum on every frame — so decoding a reference frame
/// is the test.
#[test]
fn lz4_header_checksum_matches_the_reference_encoder() {
    // Cross-check our XXH32-based header checksum against frames produced
    // by the reference encoder for several header shapes. Our decoder
    // verifies the header checksum, so a successful decode proves the
    // two implementations agree.
    for info in [
        lz4_flex::frame::FrameInfo::default(),
        {
            // A content-size field in the header exercises the optional
            // 8-byte parse path; the size must match what we write.
            let mut i = lz4_flex::frame::FrameInfo::default();
            i.content_size = Some(22);
            i
        },
        {
            // Linked blocks plus both checksum flavors.
            let mut i = lz4_flex::frame::FrameInfo::default();
            i.block_mode = lz4_flex::frame::BlockMode::Linked;
            i.block_checksums = true;
            i.content_checksum = true;
            i
        },
    ] {
        let mut enc =
            lz4_flex::frame::FrameEncoder::with_frame_info(info, Vec::<u8>::new());
        std::io::Write::write_all(&mut enc, b"checksum probe payload").unwrap();
        let data = enc.finish().unwrap();
        let out = crate::archive::lz4::decompress(&data, super::MAX_DECOMPRESSED_BYTES).unwrap();
        assert_eq!(out, b"checksum probe payload", "header checksum probe");
    }
}

/// `tar` has a long history of flag spellings (`-cvf a`, `-c -f a`, `-cfa`);
/// the parser accepts all of them so muscle memory from a host shell works.
#[test]
fn tar_parser_accepts_every_flag_shape() {
    for args in ["-cvf out.tar dir", "-cvfout.tar dir", "-cv -f out.tar dir"] {
        let parsed = parse_tar(args).unwrap();
        assert_eq!(parsed.mode, TarMode::Create, "mode for {}", args);
        assert!(parsed.verbose, "verbose for {}", args);
        assert_eq!(parsed.archive, "out.tar", "archive for {}", args);
        assert_eq!(parsed.operands, vec!["dir".to_string()], "operands for {}", args);
    }
    // Without `-v` the same command parses, just quietly.
    for args in ["-c -f out.tar dir", "-cfout.tar dir"] {
        let quiet = parse_tar(args).unwrap();
        assert_eq!(quiet.mode, TarMode::Create, "quiet form {}", args);
        assert!(!quiet.verbose, "no -v means no verbose ({})", args);
        assert_eq!(quiet.archive, "out.tar", "quiet form archive {}", args);
    }
    // Extraction with a destination, in split and attached forms.
    for args in ["-xvf out.tar -C /dest", "-xvf out.tar -C/dest", "-x -v -f out.tar -C /dest"] {
        let parsed = parse_tar(args).unwrap();
        assert_eq!(parsed.mode, TarMode::Extract, "mode for {}", args);
        assert!(parsed.verbose, "verbose for {}", args);
        assert_eq!(parsed.dest.as_deref(), Some("/dest"), "destination for {}", args);
    }
    // Listing.
    let parsed = parse_tar("-tvf out.tar").unwrap();
    assert_eq!(parsed.mode, TarMode::List, "list mode");
    assert!(parsed.verbose, "list is verbose");
    // Forced compression.
    assert_eq!(
        parse_tar("-czf out.tgz dir").unwrap().force,
        Some(TarCompress::Gzip),
        "-z forces gzip"
    );
    assert_eq!(
        parse_tar("-cJf out.txz dir").unwrap().force,
        Some(TarCompress::Xz),
        "-J forces xz"
    );
    assert_eq!(
        parse_tar("-cf out.tar dir").unwrap().force,
        None,
        "no forced compressor by default"
    );

    // Errors stay specific rather than silently doing the wrong thing.
    assert!(parse_tar("").is_err(), "empty");
    assert!(parse_tar("dir").is_err(), "no flag cluster");
    assert!(parse_tar("-cf out.tar").is_err(), "create needs operands");
    assert!(parse_tar("-cxf out.tar dir").is_err(), "only one mode");
    assert!(parse_tar("-cwf out.tar dir").is_err(), "unknown flag");
    assert!(parse_tar("-tf out.tar -C /dest").is_err(), "-C needs -x");
    assert!(parse_tar("-xf out.tar -C a -C b").is_err(), "duplicate -C");
    assert!(parse_tar("-czJf out.tar dir").is_err(), "exclusive compressors");
    assert!(parse_tar("-xf").is_err(), "-f needs a value");
    assert!(parse_tar("-xf out.tar -C").is_err(), "-C needs a value");
    // Member names may follow an extraction request (they act as a filter).
    let filtered = parse_tar("-xvf out.tar one.txt two.txt").unwrap();
    assert_eq!(filtered.operands, vec!["one.txt".to_string(), "two.txt".to_string()]);
}

/// Verbose output is a feature of every archiver, so every parser has to
/// accept `-v` (in whatever position the real tool would).
/// `tar` is the most-used verb here, and its flag grammar is the fussiest,
/// so it gets its own shape coverage on top of the shared checks below.
#[test]
fn cli_parsers_accept_verbose_everywhere() {
    let zip = parse_zip("-v out.zip a b").unwrap();
    assert!(zip.verbose, "verbose zip create");
    assert_eq!(zip.operands, vec!["a".to_string(), "b".to_string()], "zip operands");
    // Clustered flags and stored mode.
    let zip = parse_zip("-9v out.zip a").unwrap();
    assert!(zip.verbose, "clustered -9v is verbose");
    assert!(!zip.stored, "-9 means deflate");
    assert!(parse_zip("-0 out.zip a").unwrap().stored, "-0 means stored");
    assert!(parse_zip("-0v out.zip a").unwrap().stored, "clustered -0v is stored");

    let unzip = parse_unzip("-lv out.zip").unwrap();
    assert!(unzip.list_only, "clustered -lv lists");
    assert!(unzip.verbose, "clustered -lv is verbose");
    // `-d` is extraction-only.
    assert!(parse_unzip("-l out.zip -d out").is_err(), "-d only when extracting");
    let unzip = parse_unzip("-v out.zip -d out").unwrap();
    assert!(unzip.verbose, "verbose extract");
    assert_eq!(unzip.dest.as_deref(), Some("out"), "extract destination");
    assert!(parse_unzip("-v out.zip -d a -d b").is_err(), "duplicate -d");

    let sz = parse_7z("a -v out.7z dir").unwrap();
    assert_eq!(sz.mode, SevenZMode::Add, "verbose 7z add");
    assert!(sz.verbose, "verbose 7z add");
    assert_eq!(sz.operands, vec!["dir".to_string()], "7z add operands");

    let sz = parse_7z("x -v -odest out.7z").unwrap();
    assert_eq!(sz.mode, SevenZMode::Extract, "verbose 7z extract");
    assert_eq!(sz.dest.as_deref(), Some("dest"), "attached -o");
    // Split and attached destination forms.
    assert_eq!(parse_7z("x -o dest out.7z").unwrap().dest.as_deref(), Some("dest"), "split -o");

    let mfk = parse_mfk("c -v out.mfk dir").unwrap();
    assert_eq!(mfk.mode, MfkMode::Create, "verbose mfk create");
    assert!(mfk.verbose, "verbose mfk create");

    let mfk = parse_mfk("x -v -d dest out.mfk").unwrap();
    assert_eq!(mfk.mode, MfkMode::Extract, "verbose mfk extract");
    assert!(mfk.verbose, "verbose mfk extract");
    assert_eq!(mfk.dest.as_deref(), Some("dest"), "split -d value");
    assert_eq!(parse_mfk("x -ddest out.mfk").unwrap().dest.as_deref(), Some("dest"), "attached -d");

    // Every mode is reachable, and verbose works with each.
    for (verb, expected) in [
        ("a", SevenZMode::Add),
        ("x", SevenZMode::Extract),
        ("l", SevenZMode::List),
        ("t", SevenZMode::Test),
    ] {
        let parsed = parse_7z(&format!("{} -v out.7z dir", verb)).unwrap();
        assert_eq!(parsed.mode, expected, "7z mode {}", verb);
        assert!(parsed.verbose, "7z verbose {}", verb);
    }
    for (verb, expected) in [
        ("c", MfkMode::Create),
        ("x", MfkMode::Extract),
        ("l", MfkMode::List),
        ("t", MfkMode::Test),
    ] {
        let parsed = parse_mfk(&format!("{} -v out.mfk dir", verb)).unwrap();
        assert_eq!(parsed.mode, expected, "mfk mode {}", verb);
        assert!(parsed.verbose, "mfk verbose {}", verb);
    }

    // Rejections stay clear rather than silently doing the wrong thing.
    assert!(parse_7z("q out.7z").is_err(), "unknown 7z verb");
    assert!(parse_mfk("z out.mfk").is_err(), "unknown mfk verb");
    assert!(parse_7z("").is_err(), "empty 7z args");
    assert!(parse_mfk("").is_err(), "empty mfk args");
    assert!(parse_7z("x out.7z").is_ok(), "extract needs no operands");
    assert!(parse_7z("l -odest out.7z").is_err(), "-o only for x");
    assert!(parse_mfk("l -d x out.mfk").is_err(), "-d only for x");
    assert!(parse_7z("a out.7z").is_err(), "7z create needs operands");
    assert!(parse_mfk("c out.mfk").is_err(), "mfk create needs operands");
    assert!(parse_7z("x -z out.7z").is_err(), "unknown 7z flag");
    assert!(parse_mfk("x -z out.mfk").is_err(), "unknown mfk flag");
    assert!(parse_7z("x").is_err(), "7z needs an archive");
    assert!(parse_7z("x -o").is_err(), "-o needs a directory");
    assert!(parse_mfk("x -d").is_err(), "-d needs a directory");
    assert!(parse_7z("x -oa -ob out.7z").is_err(), "duplicate -o");
    assert!(parse_mfk("x -da -db out.mfk").is_err(), "duplicate -d");
    assert!(parse_zip("-q out.zip a").is_err(), "unknown zip flag");
    assert!(parse_unzip("-q out.zip").is_err(), "unknown unzip flag");
    assert!(parse_unzip("").is_err(), "unzip needs an archive");
    assert!(parse_zip("out.zip").is_err(), "zip needs operands");
    assert!(parse_zip("").is_err(), "zip needs an archive");

    // Real 7-Zip takes options on either side of the archive name, so
    // `7z x a.7z -odest` must extract into `dest` rather than treating
    // `-odest` as a member name.
    let parsed = parse_7z("x out.7z -odest dir").unwrap();
    assert_eq!(parsed.dest.as_deref(), Some("dest"), "trailing -o");
    assert_eq!(parsed.operands, vec!["dir".to_string()], "trailing -o keeps operands");
    let parsed = parse_mfk("x out.mfk notes.txt -d dest -v").unwrap();
    assert_eq!(parsed.dest.as_deref(), Some("dest"), "trailing -d");
    assert!(parsed.verbose, "trailing -v");
    assert_eq!(parsed.operands, vec!["notes.txt".to_string()], "trailing -d keeps operands");
    assert!(parse_7z("x out.7z -z").is_err(), "unknown trailing 7z flag");
    assert!(parse_mfk("x out.mfk -z").is_err(), "unknown trailing mfk flag");
    assert!(parse_7z("x out.7z -oa -ob").is_err(), "duplicate trailing -o");
}

/// `tar -z` / `-J` must beat the output extension: with a plain `.tar` name
/// the forced compressor decides, and the archive is still detected by magic.
#[test]
fn forced_tar_compression_overrides_the_extension() {
    let entries = [BuildEntry { name: "a.txt", data: Some(b"forced") }];

    let bytes = build_tar_auto(&entries, "out.tar", Some(crate::archive::TarCompress::Gzip)).unwrap();
    assert_eq!(
        detect(&bytes, "out.tar").unwrap(),
        crate::archive::Kind::TarGzip,
        "-z produces gzip regardless of the name"
    );
    let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut out).unwrap();
    let read = tar::read_entries(&out, super::MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(&read[0].data[..], b"forced", "payload survives forced gzip");

    let bytes = build_tar_auto(&entries, "out.tar", Some(crate::archive::TarCompress::Xz)).unwrap();
    assert_eq!(
        detect(&bytes, "out.tar").unwrap(),
        crate::archive::Kind::TarXz,
        "-J produces xz regardless of the name"
    );

    // An explicit forced compressor still loses to the decode-only
    // extensions, which have no encoder here.
    assert!(
        build_tar_auto(&entries, "out.tar.bz2", Some(crate::archive::TarCompress::Gzip)).is_err(),
        "forced gzip does not sneak past the bz2 refusal"
    );

    // Without a flag the extension decides, as before.
    assert_eq!(
        detect(&build_tar_auto(&entries, "out.tgz", None).unwrap(), "out.tgz").unwrap(),
        crate::archive::Kind::TarGzip,
        ".tgz still means gzip"
    );
}

/// Corrupting a packed 7z stream must be caught by the pack-stream CRC
/// before the folder is decoded, not silently produce garbage.
#[test]
fn sevenz_pack_crc_catches_a_corrupt_stream() {
    let entries = [
        BuildEntry { name: "a.txt", data: Some(b"payload that compresses well aaaaaaaaaaaaaaaaaaaa") },
        BuildEntry { name: "b.txt", data: Some(b"second member") },
    ];
    let data = sevenz::build_with(&entries, sevenz::Method::Lzma2).unwrap();
    assert_eq!(sevenz::extract(&data, super::MAX_DECOMPRESSED_BYTES).unwrap().len(), 2, "clean archive");

    // Packed streams start right after the 32-byte signature header.
    let mut corrupt = data.clone();
    let victim = 32;
    corrupt[victim] ^= 0xFF;
    let err = sevenz::extract(&corrupt, super::MAX_DECOMPRESSED_BYTES).unwrap_err();
    assert!(
        err.contains("pack stream crc"),
        "pack stream CRC catches the damage before decoding, got {:?}",
        err
    );
}
