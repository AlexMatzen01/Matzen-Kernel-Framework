//! Host-side filesystem image tests (`cargo test -p mfk-kernel`).
//!
//! These tests mount real Linux-created images (see
//! `tools/make_fs_test_images.sh`) with the native MFK drivers and verify
//! byte-exact reads, links, directories and write round-trips. The MFK
//! drivers parse the images independently; nothing here reuses the host
//! filesystem code that created them.
//!
//! The tests skip gracefully when the images are absent so a plain
//! `cargo test` stays green; run `tools/make_fs_test_images.sh` first for
//! full coverage. After the write round-trips, the mutated copies
//! (`ext4-mutated.img`, `exfat-mutated.img`) are written back next to the
//! inputs for `e2fsck` / `fsck.exfat` verification on the host.

use alloc::vec::Vec;

use crate::drivers::block::{BlockDevice, RamDisk};
use crate::fs::error::FsError;
use crate::fs::exfat::ExFat;
use crate::fs::ext4::Ext4;
use crate::fs::partition::{PartTable, PartitionTable};

const EXT4_IMG: &str = "ext4.img";
const EXFAT_IMG: &str = "exfat.img";

/// Byte `i` of the `gen_pattern.py` reference files.
fn pattern_byte(i: usize) -> u8 {
    (i % 251) as u8
}

fn expected_pattern(len: usize) -> Vec<u8> {
    (0..len).map(pattern_byte).collect()
}

fn image_dir() -> Option<std::path::PathBuf> {
    if let Ok(d) = std::env::var("MFK_FS_TEST_DIR") {
        let p = std::path::PathBuf::from(d);
        if p.is_dir() {
            return Some(p);
        }
    }
    for candidate in ["target/fs-test", "../target/fs-test"] {
        let p = std::path::PathBuf::from(candidate);
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

fn load_image(name: &str) -> Option<(std::path::PathBuf, Vec<u8>)> {
    let dir = image_dir()?;
    let path = dir.join(name);
    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() % 512 != 0 || bytes.is_empty() {
        return None;
    }
    Some((dir, bytes))
}

fn ramdisk_from(bytes: &[u8]) -> RamDisk {
    let blocks = (bytes.len() / 512) as u64;
    let mut disk = RamDisk::new(blocks);
    // RamDisk has no per-call block cap, but stay well under any limit.
    for (i, chunk) in bytes.chunks(512 * 256).enumerate() {
        let blocks = chunk.len() / 512;
        disk.write_blocks(i as u64 * 256, blocks, chunk).unwrap();
    }
    disk
}

fn dump_disk(disk: &mut RamDisk, dir: &std::path::Path, name: &str) {
    let blocks = disk.block_count() as usize;
    let mut buf = alloc::vec![0u8; blocks * 512];
    if disk.read_blocks(0, blocks, &mut buf).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join(name), &buf);
}

fn require_image(name: &str) -> Option<(std::path::PathBuf, RamDisk)> {
    let (dir, bytes) = load_image(name)?;
    Some((dir, ramdisk_from(&bytes)))
}

// ── ext4 ─────────────────────────────────────────────────────────────

#[test]
fn ext4_probe_accepts_linux_image_and_rejects_others() {
    let Some((_dir, mut disk)) = require_image(EXT4_IMG) else {
        return;
    };
    assert!(Ext4::probe(&mut disk));
    assert!(!ExFat::probe(&mut disk));
    // SimplFS magic ("SFMK") must not match an ext4 superblock.
    let mut sb = [0u8; 512];
    disk.read_blocks(0, 1, &mut sb).unwrap();
    assert_ne!(u32::from_le_bytes([sb[0], sb[1], sb[2], sb[3]]), 0x53464D4B);
}

#[test]
fn ext4_mounts_linux_image_and_reads_files() {
    let Some((_dir, mut disk)) = require_image(EXT4_IMG) else {
        return;
    };
    let mut fs = Ext4::mount(&mut disk).expect("mount Linux ext4 image");
    assert!(!fs.is_read_only(), "fresh image must mount read-write");

    let hello = {
        let ino = fs.resolve(&mut disk, "/hello.txt").expect("hello.txt");
        let stat = fs.stat(&mut disk, ino).expect("stat");
        assert!(stat.is_reg);
        assert_eq!(stat.size, 15);
        assert_eq!(stat.mode & 0o777, 0o644);
        let mut buf = alloc::vec![0u8; stat.size as usize];
        let n = fs.read_at(&mut disk, ino, 0, &mut buf).unwrap();
        assert_eq!(n, 15);
        buf
    };
    assert_eq!(hello, b"hello-mfk-ext4\n");

    let deep_ino = fs.resolve(&mut disk, "/docs/nested/deep.txt").expect("deep.txt");
    let mut deep = alloc::vec![0u8; 32];
    let n = fs.read_at(&mut disk, deep_ino, 0, &mut deep).unwrap();
    deep.truncate(n);
    assert_eq!(deep, b"deep-content-123\n");

    // Root listing contains everything debugfs seeded.
    let names: Vec<String> = fs
        .dir_entries(&mut disk, 2)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    for want in ["hello.txt", "big.bin", "link.txt", "hard.txt", "docs"] {
        assert!(names.iter().any(|n| n == want), "missing {}", want);
    }
}

#[test]
fn ext4_reads_big_linux_file_byte_exact() {
    let Some((_dir, mut disk)) = require_image(EXT4_IMG) else {
        return;
    };
    let mut fs = Ext4::mount(&mut disk).unwrap();
    let ino = fs.resolve(&mut disk, "/big.bin").unwrap();
    let stat = fs.stat(&mut disk, ino).unwrap();
    assert_eq!(stat.size, 5 * 1024 * 1024);
    let mut buf = alloc::vec![0u8; stat.size as usize];
    let mut done = 0usize;
    while done < buf.len() {
        // Stream in 1 MiB windows to exercise multi-extent offset reads.
        let end = (done + (1 << 20)).min(buf.len());
        let n = fs.read_at(&mut disk, ino, done as u64, &mut buf[done..end]).unwrap();
        assert!(n > 0);
        done += n;
    }
    assert_eq!(buf, expected_pattern(5 * 1024 * 1024));
}

#[test]
fn ext4_symlink_and_hardlink_from_linux() {
    let Some((_dir, mut disk)) = require_image(EXT4_IMG) else {
        return;
    };
    let mut fs = Ext4::mount(&mut disk).unwrap();

    let link = fs.resolve(&mut disk, "/link.txt").unwrap();
    let stat = fs.stat(&mut disk, link).unwrap();
    assert!(stat.is_lnk);
    assert_eq!(fs.read_link(&mut disk, link).unwrap(), "hello.txt");

    let hard = fs.resolve(&mut disk, "/hard.txt").unwrap();
    let hello = fs.resolve(&mut disk, "/hello.txt").unwrap();
    assert_eq!(hard, hello, "hard link shares the inode");
    let stat = fs.stat(&mut disk, hard).unwrap();
    assert_eq!(stat.links, 2);
}

#[test]
fn ext4_trait_level_guest_sequence() {
    use crate::fs::Filesystem;
    let Some((_dir, mut disk)) = require_image(EXT4_IMG) else {
        return;
    };
    let payload: Vec<u8> = (0..16usize).map(|i| (i * 7 % 251) as u8).collect();
    {
        let mut fs = Ext4::mount(&mut disk).unwrap();
        // Mirror the guest: mkdir, write (create+write), symlink, cp.
        <Ext4 as Filesystem>::create_dir(&mut fs, &mut disk, "/mfk-boot").unwrap();
        <Ext4 as Filesystem>::create_file(&mut fs, &mut disk, "/mfk-boot/note.txt").unwrap();
        <Ext4 as Filesystem>::write_file(&mut fs, &mut disk, "/mfk-boot/note.txt", &payload).unwrap();
        <Ext4 as Filesystem>::symlink(&mut fs, &mut disk, "/mfk-link", "/hello.txt").unwrap();
        let data = <Ext4 as Filesystem>::read_file(&mut fs, &mut disk, "/hello.txt").unwrap();
        <Ext4 as Filesystem>::create_file(&mut fs, &mut disk, "/mfk-boot/copy.txt").unwrap();
        <Ext4 as Filesystem>::write_file(&mut fs, &mut disk, "/mfk-boot/copy.txt", &data).unwrap();
    }
    // Remount and verify every byte from disk.
    let mut fs = Ext4::mount(&mut disk).unwrap();
    let back = <Ext4 as Filesystem>::read_file(&mut fs, &mut disk, "/mfk-boot/note.txt").unwrap();
    assert_eq!(back, payload);
    let entries = <Ext4 as Filesystem>::list_dir(&mut fs, &mut disk, "/mfk-boot").unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"note.txt"), "note.txt missing: {:?}", names);
    assert!(names.contains(&"copy.txt"), "copy.txt missing: {:?}", names);
}

#[test]
fn ext4_subdir_file_roundtrip() {    let Some((_dir, mut disk)) = require_image(EXT4_IMG) else {
        return;
    };
    let payload: Vec<u8> = (0..5000usize).map(|i| (i * 3 % 251) as u8).collect();
    {
        let mut fs = Ext4::mount(&mut disk).unwrap();
        let root = fs.resolve(&mut disk, "/").unwrap();
        let sub = fs.mkdir(&mut disk, root, "sub").unwrap();
        let ino = fs.create(&mut disk, sub, "inner.txt").unwrap();
        fs.append(&mut disk, ino, &payload).unwrap();
    }
    let mut fs = Ext4::mount(&mut disk).unwrap();
    let ino = fs.resolve(&mut disk, "/sub/inner.txt").unwrap();
    let stat = fs.stat(&mut disk, ino).unwrap();
    assert_eq!(stat.size, payload.len() as u64);
    let mut back = alloc::vec![0u8; payload.len()];
    let n = fs.read_at(&mut disk, ino, 0, &mut back).unwrap();
    assert_eq!(n, payload.len());
    assert_eq!(back, payload);
}

#[test]
fn ext4_write_roundtrip_survives_remount() {
    let Some((dir, mut disk)) = require_image(EXT4_IMG) else {
        return;
    };
    let payload: Vec<u8> = (0..90_000usize).map(|i| (i * 7 % 251) as u8).collect();
    let ino = {
        let mut fs = Ext4::mount(&mut disk).unwrap();
        let root = fs.resolve(&mut disk, "/").unwrap();
        let ino = fs.create(&mut disk, root, "mfk-test.txt").unwrap();
        fs.append(&mut disk, ino, &payload).unwrap();
        fs.mkdir(&mut disk, root, "mfk-dir").unwrap();
        fs.rename(&mut disk, "/mfk-test.txt", "/mfk-dir/renamed.txt").unwrap();
        ino
    };
    // Re-mount from the same bytes and verify.
    let mut fs = Ext4::mount(&mut disk).unwrap();
    let moved = fs.resolve(&mut disk, "/mfk-dir/renamed.txt").unwrap();
    assert_eq!(moved, ino);
    let stat = fs.stat(&mut disk, moved).unwrap();
    assert_eq!(stat.size, payload.len() as u64);
    let mut back = alloc::vec![0u8; payload.len()];
    let n = fs.read_at(&mut disk, moved, 0, &mut back).unwrap();
    assert_eq!(n, payload.len());
    assert_eq!(back, payload);
    // Clean up so the mutated image stays close to the input.
    fs.unlink(&mut disk, "/mfk-dir/renamed.txt").unwrap();
    fs.rmdir(&mut disk, "/mfk-dir").unwrap();
    assert!(fs.resolve(&mut disk, "/mfk-dir/renamed.txt").is_err());
    dump_disk(&mut disk, &dir, "ext4-mutated.img");
}

// ── exFAT ────────────────────────────────────────────────────────────

#[test]
fn exfat_probe_accepts_linux_image_and_rejects_others() {
    let Some((_dir, mut disk)) = require_image(EXFAT_IMG) else {
        return;
    };
    assert!(ExFat::probe(&mut disk));
    assert!(!Ext4::probe(&mut disk));
}

#[test]
fn exfat_mounts_linux_image_and_reads_files() {
    let Some((_dir, mut disk)) = require_image(EXFAT_IMG) else {
        return;
    };
    let mut fs = ExFat::mount(&mut disk).expect("mount Linux exFAT image");

    let data = fs.read_file(&mut disk, "/greeting.txt").expect("greeting");
    assert_eq!(data, b"exfat hello\n");

    // exFAT lookups are case-insensitive.
    let upper = fs.read_file(&mut disk, "/GREETING.TXT").expect("upper");
    assert_eq!(upper, data);

    let long = fs
        .read_file(&mut disk, "/docs and nested dirs/This is a very long filename for exFAT testing.txt")
        .expect("long name");
    assert_eq!(long, b"deep exfat content 456\n");
}

#[test]
fn exfat_reads_big_linux_file_byte_exact() {
    let Some((_dir, mut disk)) = require_image(EXFAT_IMG) else {
        return;
    };
    let mut fs = ExFat::mount(&mut disk).unwrap();
    let data = fs.read_file(&mut disk, "/frag-big.bin").unwrap();
    assert_eq!(data.len(), 8 * 1024 * 1024);
    assert_eq!(data, expected_pattern(8 * 1024 * 1024));
}

#[test]
fn exfat_rename_and_rmdir_roundtrip() {
    let Some((_dir, mut disk)) = require_image(EXFAT_IMG) else {
        return;
    };
    {
        let mut fs = ExFat::mount(&mut disk).unwrap();
        fs.mkdir_path(&mut disk, "/mfk-tmp").unwrap();
        fs.rename(&mut disk, "/greeting.txt", "/mfk-tmp/moved.txt").unwrap();
        assert!(fs.resolve_path(&mut disk, "/greeting.txt").is_err());
        let back = fs.read_file(&mut disk, "/mfk-tmp/moved.txt").unwrap();
        assert_eq!(back, b"exfat hello\n");
        // Non-empty dir refuses removal; empty dir goes away.
        assert!(fs.remove_dir(&mut disk, "/mfk-tmp").is_err());
        fs.delete_file(&mut disk, "/mfk-tmp/moved.txt").unwrap();
        fs.remove_dir(&mut disk, "/mfk-tmp").unwrap();
        assert!(fs.resolve_path(&mut disk, "/mfk-tmp").is_err());
    }
    // Nothing must be lost: the rest of the volume still reads.
    let mut fs = ExFat::mount(&mut disk).unwrap();
    let names: Vec<String> = fs
        .list_dir(&mut disk, "/")
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert!(names.iter().any(|n| n == "frag-big.bin"));
}

#[test]
fn exfat_fragmented_write_then_read_across_remount() {
    let Some((dir, mut disk)) = require_image(EXFAT_IMG) else {
        return;
    };
    // Free the scattered fill files; the allocator first-fits from cluster 2,
    // so the replacement file cannot be contiguous (holes are 64 clusters,
    // the file needs ~512).
    let payload = expected_pattern(2 * 1024 * 1024);
    {
        let mut fs = ExFat::mount(&mut disk).unwrap();
        for f in fs.list_dir(&mut disk, "/").unwrap() {
            if f.name.starts_with("fill-") {
                fs.delete_file(&mut disk, &alloc::format!("/{}", f.name)).unwrap();
            }
        }
        fs.write_file(&mut disk, "/mfk-frag.bin", &payload).unwrap();
    }
    // Fresh mount forces a full FAT re-walk from disk.
    let mut fs = ExFat::mount(&mut disk).unwrap();
    let back = fs.read_file(&mut disk, "/mfk-frag.bin").unwrap();
    assert_eq!(back, payload);
    // Offset reads must agree across cluster boundaries too.
    let stat = fs.stat_path(&mut disk, "/mfk-frag.bin").unwrap();
    assert_eq!(stat.size, payload.len() as u64);
    dump_disk(&mut disk, &dir, "exfat-mutated.img");
}

// ── partitions ───────────────────────────────────────────────────────

#[test]
fn test_images_are_whole_disk_volumes() {
    for name in [EXT4_IMG, EXFAT_IMG] {
        let Some((_dir, mut disk)) = require_image(name) else {
            return;
        };
        let table = PartitionTable::read(&mut disk);
        assert_eq!(
            table.table,
            PartTable::WholeDisk,
            "{} should have no partition table",
            name
        );
        assert_eq!(table.partitions.len(), 1);
    }
}

#[test]
fn unknown_format_is_rejected_by_every_probe() {
    let mut disk = RamDisk::new(128);
    let zero = [0u8; 512];
    disk.write_blocks(0, 1, &zero).unwrap();
    assert!(!Ext4::probe(&mut disk));
    assert!(!ExFat::probe(&mut disk));
    assert!(matches!(Ext4::mount(&mut disk), Err(FsError::WrongFs)));
    assert!(matches!(ExFat::mount(&mut disk), Err(FsError::WrongFs)));
}
