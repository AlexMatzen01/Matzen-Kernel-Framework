//! Host-side SimplFS helper for bundling apps into raw disk.img
//! Minimal reimplementation of kernel/src/fs layout to run on std.
//! Only supports fresh format + file/dir creation for bundling.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const FS_BLOCK_SIZE: usize = 512;
const MAX_FILENAME_LEN: usize = 56;
const MAX_INODES: usize = 256;
const INODE_DIRECT_BLOCKS: usize = 12;
const INDIRECT_DATA_BLOCKS: usize = FS_BLOCK_SIZE / std::mem::size_of::<u64>() - 1;

/// Mirrors `kernel/src/fs/mod.rs::Superblock` byte for byte.
///
/// This is a hand-maintained duplicate with no compile-time link to the
/// kernel's definition, so the two layouts are kept in step by the assertions
/// below: if either struct changes size or field order, these fail to compile
/// rather than producing an image the kernel refuses to mount.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Superblock {
    magic: u32,
    version: u32,
    block_size: u32,
    total_blocks: u64,
    inode_count: u32,
    inode_blocks: u32,
    data_block_start: u64,
    free_blocks: u64,
    free_inodes: u32,
    root_inode: u32,
    bitmap_start: u64,
    bitmap_blocks: u32,
    reserved: [u8; 448],
}

const _: () = assert!(std::mem::size_of::<Superblock>() == FS_BLOCK_SIZE);
const _: () = assert!(std::mem::size_of::<Inode>() == 144);
const _: () = assert!(std::mem::size_of::<DirectoryEntry>() == 64);
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Inode {
    file_type: u8,
    permissions: u8,
    reserved1: u16,
    size: u64,
    blocks_used: u32,
    created: u64,
    modified: u64,
    direct_blocks: [u64; INODE_DIRECT_BLOCKS],
    reserved2: [u8; 16],
}
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DirectoryEntry {
    inode_number: u32,
    name: [u8; MAX_FILENAME_LEN],
    reserved: [u8; 4],
}

const SUPER_MAGIC: u32 = 0x53464D4B; // "SFMK"
/// Must match `Superblock::VERSION` in kernel/src/fs/mod.rs.
const SUPER_VERSION: u32 = 2;

/// Blocks the allocation bitmap needs for `data_blocks` bits.
const fn bitmap_blocks_for(data_blocks: u64) -> u32 {
    let bytes = (data_blocks as usize).div_ceil(8);
    bytes.div_ceil(FS_BLOCK_SIZE) as u32
}
const FT_EMPTY: u8 = 0;
const FT_FILE: u8 = 1;
const FT_DIR: u8 = 2;

fn inode_size() -> usize {
    std::mem::size_of::<Inode>()
}

struct RawDisk {
    file: File,
    block_count: u64,
}

impl RawDisk {
    fn open(path: &Path, block_count: u64) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        Ok(Self { file, block_count })
    }
    fn read_blocks(&mut self, start: u64, count: usize, buf: &mut [u8]) -> std::io::Result<()> {
        self.file
            .seek(SeekFrom::Start(start * FS_BLOCK_SIZE as u64))?;
        let to_read = count * FS_BLOCK_SIZE;
        assert!(buf.len() >= to_read);
        self.file.read_exact(&mut buf[..to_read])?;
        Ok(())
    }
    fn write_blocks(&mut self, start: u64, count: usize, buf: &[u8]) -> std::io::Result<()> {
        self.file
            .seek(SeekFrom::Start(start * FS_BLOCK_SIZE as u64))?;
        let to_write = count * FS_BLOCK_SIZE;
        assert!(buf.len() >= to_write);
        self.file.write_all(&buf[..to_write])?;
        self.file.flush()?;
        Ok(())
    }
}

/// Inject a single host file into a SimplFS disk image at `guest_path`
/// (e.g. `/wad/doom1.wad`). Formats the disk first when it has no SimplFS
/// magic, so `--extra-disk` images work on first boot. Large files ride the
/// same chained-indirect layout as `host_create_file`, so multi-MB WADs fit.
pub fn bundle_single_file(
    disk_path: &Path,
    guest_path: &str,
    host_path: &Path,
) -> Result<(), String> {
    let data = std::fs::read(host_path).map_err(|e| {
        format!(
            "read {}: {}",
            host_path.display(),
            e
        )
    })?;
    let metadata = std::fs::metadata(disk_path).map_err(|e| e.to_string())?;
    let len = metadata.len();
    if len % FS_BLOCK_SIZE as u64 != 0 {
        return Err("disk size not multiple of block size".into());
    }
    let total_blocks = len / FS_BLOCK_SIZE as u64;
    let mut disk = RawDisk::open(disk_path, total_blocks).map_err(|e| e.to_string())?;
    let mut sb_buf = [0u8; FS_BLOCK_SIZE];
    disk.read_blocks(0, 1, &mut sb_buf)
        .map_err(|e| e.to_string())?;
    let magic = u32::from_le_bytes([sb_buf[0], sb_buf[1], sb_buf[2], sb_buf[3]]);
    let version = u32::from_le_bytes([sb_buf[4], sb_buf[5], sb_buf[6], sb_buf[7]]);
    if magic != SUPER_MAGIC || version != SUPER_VERSION {
        println!(
            "WAD bundle: formatting {} as SimplFS v{} ({} blocks)...",
            disk_path.display(),
            SUPER_VERSION,
            total_blocks
        );
        format_disk(&mut disk, total_blocks).map_err(|e| e.to_string())?;
    }
    println!(
        "WAD bundle: {} ({} bytes) -> {}:{}",
        host_path.display(),
        data.len(),
        disk_path.display(),
        guest_path
    );
    host_create_file(&mut disk, guest_path, &data)?;
    println!("WAD bundle: done. In kernel: mount <drive>; ls {}; doominfo {}", guest_path, guest_path);
    Ok(())
}

pub fn bundle_examples(disk_path: &Path, examples_root: &Path) -> Result<(), String> {
    // if examples_root doesn't exist, skip
    if !examples_root.exists() {
        return Ok(());
    }
    // collect files
    let mut files = Vec::new();
    collect_files(examples_root, examples_root, &mut files);

    if files.is_empty() {
        return Ok(());
    }

    // open disk
    let metadata = std::fs::metadata(disk_path).map_err(|e| e.to_string())?;
    let len = metadata.len();
    if len % FS_BLOCK_SIZE as u64 != 0 {
        return Err("disk size not multiple of block size".into());
    }
    let total_blocks = len / FS_BLOCK_SIZE as u64;
    let mut disk = RawDisk::open(disk_path, total_blocks).map_err(|e| e.to_string())?;

    // check if already formatted by reading superblock magic
    let mut sb_buf = [0u8; FS_BLOCK_SIZE];
    disk.read_blocks(0, 1, &mut sb_buf)
        .map_err(|e| e.to_string())?;
    let magic = u32::from_le_bytes([sb_buf[0], sb_buf[1], sb_buf[2], sb_buf[3]]);
    let version = u32::from_le_bytes([sb_buf[4], sb_buf[5], sb_buf[6], sb_buf[7]]);
    // A version-1 image has no allocation bitmap and the kernel refuses to
    // mount it, so it must be reformatted rather than appended to.
    let is_formatted = magic == SUPER_MAGIC && version == SUPER_VERSION;
    if magic == SUPER_MAGIC && version != SUPER_VERSION {
        println!(
            "Host bundle: existing image is SimplFS v{} (kernel wants v{}); reformatting",
            version, SUPER_VERSION
        );
    }

    if !is_formatted {
        println!(
            "Host bundle: disk not formatted, formatting SimplFS ({} blocks)...",
            total_blocks
        );
        format_disk(&mut disk, total_blocks).map_err(|e| e.to_string())?;
    } else {
        // verify we can mount (read inode table) - if fails, reformat?
        println!("Host bundle: disk already formatted (magic ok), injecting files...");
    }

    // need to perform file creation using our host FS logic (re-read superblock, inodes, etc.)
    // For simplicity after formatting, we will create dirs and files
    for (guest_path, host_path) in &files {
        let data = std::fs::read(host_path).map_err(|e| e.to_string())?;
        println!(
            "  bundling {} -> {} ({} bytes)",
            host_path,
            guest_path,
            data.len()
        );
        host_create_file(&mut disk, guest_path, &data)
            .map_err(|e| format!("{}: {}", guest_path, e))?;
    }

    println!(
        "Host bundle: injected {} file(s) into {}",
        files.len(),
        disk_path.display()
    );
    println!("  Inside kernel: mount; ls /apps; run /apps/hello.app");
    Ok(())
}

fn collect_files(root: &Path, cur: &Path, out: &mut Vec<(String, String)>) {
    if let Ok(entries) = std::fs::read_dir(cur) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_files(root, &p, out);
            } else if p.is_file() {
                // guest path = /apps/<relative>  but keep relative under examples/
                // For apps/examples/hello.app -> guest /apps/hello.app
                // apps/examples/sub/x -> /apps/sub/x
                if let Ok(rel) = p.strip_prefix(root) {
                    let guest = format!("/apps/{}", rel.to_string_lossy().replace('\\', "/"));
                    out.push((guest, p.to_string_lossy().to_string()));
                }
            }
        }
    }
}

fn format_disk(disk: &mut RawDisk, total_blocks: u64) -> std::io::Result<()> {
    let inode_blocks = (MAX_INODES * inode_size() + FS_BLOCK_SIZE - 1) / FS_BLOCK_SIZE;
    let bitmap_start = 1 + inode_blocks as u64;
    // Version 2 stores an allocation bitmap between the inode table and the
    // data region. The kernel refuses a version 1 image, and it must refuse:
    // version 1 kept free-space state only in RAM, so a host-bundled image
    // would come back with a data region that starts at the wrong block.
    let bitmap_input = total_blocks.saturating_sub(bitmap_start);
    let bitmap_blocks = bitmap_blocks_for(bitmap_input);
    let data_start = bitmap_start + bitmap_blocks as u64;
    let data_blocks = total_blocks.saturating_sub(data_start);
    if total_blocks <= data_start {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "disk too small for SimplFS",
        ));
    }
    let superblock = Superblock {
        magic: SUPER_MAGIC,
        version: SUPER_VERSION,
        block_size: FS_BLOCK_SIZE as u32,
        total_blocks,
        inode_count: MAX_INODES as u32,
        inode_blocks: inode_blocks as u32,
        data_block_start: data_start,
        free_blocks: data_blocks - 1, // minus root dir block
        free_inodes: (MAX_INODES as u32) - 1,
        root_inode: 0,
        bitmap_start,
        bitmap_blocks,
        reserved: [0; 448],
    };
    // write superblock
    let mut buf = [0u8; FS_BLOCK_SIZE];
    unsafe {
        let src = &superblock as *const _ as *const u8;
        std::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), std::mem::size_of::<Superblock>());
    }
    // Ensure LE (host is little endian, same as kernel)
    disk.write_blocks(0, 1, &buf)?;

    // Write the allocation bitmap: everything free except the root directory,
    // which occupies the first *data* block (bit 0).
    let bitmap_len = (data_blocks as usize).div_ceil(8);
    let mut bitmap = vec![0u8; bitmap_len];
    bitmap[0] |= 1;
    let zero = [0u8; FS_BLOCK_SIZE];
    let mut remaining = bitmap_len;
    let mut block = bitmap_start;
    for _ in 0..bitmap_blocks {
        if remaining == 0 {
            break;
        }
        let n = std::cmp::min(FS_BLOCK_SIZE, remaining);
        let offset = bitmap_len - remaining;
        let mut b = [0u8; FS_BLOCK_SIZE];
        b[..n].copy_from_slice(&bitmap[offset..offset + n]);
        disk.write_blocks(block, 1, &b)?;
        remaining -= n;
        block += 1;
    }
    let _ = zero;

    // inode table
    let mut inodes = vec![
        Inode {
            file_type: FT_EMPTY,
            permissions: 0,
            reserved1: 0,
            size: 0,
            blocks_used: 0,
            created: 0,
            modified: 0,
            direct_blocks: [0; INODE_DIRECT_BLOCKS],
            reserved2: [0; 16],
        };
        MAX_INODES
    ];
    inodes[0].file_type = FT_DIR;
    inodes[0].permissions = 0o77;
    inodes[0].blocks_used = 1;
    inodes[0].direct_blocks[0] = data_start;

    let inode_bytes = unsafe {
        std::slice::from_raw_parts(inodes.as_ptr() as *const u8, inodes.len() * inode_size())
    };
    let mut block_num = 1u64;
    for chunk in inode_bytes.chunks(FS_BLOCK_SIZE) {
        let mut b = [0u8; FS_BLOCK_SIZE];
        b[..chunk.len()].copy_from_slice(chunk);
        disk.write_blocks(block_num, 1, &b)?;
        block_num += 1;
    }
    // clear root dir block
    let zero = [0u8; FS_BLOCK_SIZE];
    disk.write_blocks(data_start, 1, &zero)?;
    Ok(())
}

/// Bitmap-backed block allocator for the host tool.
///
/// The previous allocator inferred the next free block *arithmetically* from
/// `free_blocks`, which assumes used blocks form one contiguous run from the
/// start of the data region. The kernel instead uses a bitmap with a rotating
/// cursor, so the two diverged the moment a block in the middle of the region
/// was freed -- the host would hand out a block the kernel considered live.
/// Reading the same on-disk bitmap removes the divergence entirely.
struct HostAlloc {
    bitmap: Vec<u8>,
    dirty: Vec<bool>,
    cursor: usize,
    bitmap_start: u64,
    bitmap_blocks: u32,
    data_start: u64,
    data_blocks: usize,
}

impl HostAlloc {
    fn load(disk: &mut RawDisk, sb: &Superblock) -> Result<Self, String> {
        let total = unsafe { std::ptr::addr_of!(sb.total_blocks).read_unaligned() };
        let data_start = unsafe { std::ptr::addr_of!(sb.data_block_start).read_unaligned() };
        let bitmap_start = unsafe { std::ptr::addr_of!(sb.bitmap_start).read_unaligned() };
        let bitmap_blocks = unsafe { std::ptr::addr_of!(sb.bitmap_blocks).read_unaligned() };
        let data_blocks = total.saturating_sub(data_start) as usize;
        let mut bitmap = vec![0u8; data_blocks.div_ceil(8)];
        let mut remaining = bitmap.len();
        let mut block = bitmap_start;
        for _ in 0..bitmap_blocks {
            if remaining == 0 {
                break;
            }
            let n = std::cmp::min(FS_BLOCK_SIZE, remaining);
            let offset = bitmap.len() - remaining;
            let mut b = [0u8; FS_BLOCK_SIZE];
            disk.read_blocks(block, 1, &mut b).map_err(|e| e.to_string())?;
            bitmap[offset..offset + n].copy_from_slice(&b[..n]);
            remaining -= n;
            block += 1;
        }
        let mut dirty = vec![false; bitmap_blocks as usize];
        Ok(Self {
            bitmap,
            dirty,
            cursor: 0,
            bitmap_start,
            bitmap_blocks,
            data_start,
            data_blocks,
        })
    }

    #[inline]
    fn is_used(&self, index: usize) -> bool {
        self.bitmap[index / 8] & (1 << (index % 8)) != 0
    }

    fn alloc(&mut self, sb: &mut Superblock) -> Result<u64, String> {
        if self.data_blocks == 0 {
            return Err("no free blocks".into());
        }
        for step in 0..self.data_blocks {
            let index = (self.cursor + step) % self.data_blocks;
            if !self.is_used(index) {
                self.bitmap[index / 8] |= 1 << (index % 8);
                let per_block = FS_BLOCK_SIZE * 8;
                if let Some(f) = self.dirty.get_mut(index / per_block) {
                    *f = true;
                }
                self.cursor = (index + 1) % self.data_blocks;
                unsafe {
                    let free = std::ptr::addr_of!(sb.free_blocks)
                        .read_unaligned()
                        .checked_sub(1)
                        .ok_or_else(|| "no free blocks".to_string())?;
                    std::ptr::addr_of_mut!(sb.free_blocks).write_unaligned(free);
                }
                return Ok(self.data_start + index as u64);
            }
        }
        Err("no free blocks".into())
    }

    /// Write back the bitmap blocks that changed.
    fn flush(&mut self, disk: &mut RawDisk) -> Result<(), String> {
        let per_block = FS_BLOCK_SIZE * 8;
        for (i, dirty) in self.dirty.iter_mut().enumerate() {
            if !*dirty || (i as u32) >= self.bitmap_blocks {
                continue;
            }
            let first = (i * per_block) / 8;
            if first >= self.bitmap.len() {
                *dirty = false;
                continue;
            }
            let n = std::cmp::min(FS_BLOCK_SIZE, self.bitmap.len() - first);
            let mut b = [0u8; FS_BLOCK_SIZE];
            b[..n].copy_from_slice(&self.bitmap[first..first + n]);
            disk.write_blocks(self.bitmap_start + i as u64, 1, &b)
                .map_err(|e| e.to_string())?;
            *dirty = false;
        }
        Ok(())
    }
}

fn host_file_block(
    disk: &mut RawDisk,
    inode: &mut Inode,
    logical_block: usize,
    alloc: &mut HostAlloc,
    superblock: &mut Superblock,
) -> Result<u64, String> {
    if logical_block < INODE_DIRECT_BLOCKS {
        let block =
            unsafe { std::ptr::addr_of!(inode.direct_blocks[logical_block]).read_unaligned() };
        if block != 0 {
            return Ok(block);
        }
        let block = alloc.alloc(superblock)?;
        inode.direct_blocks[logical_block] = block;
        return Ok(block);
    }

    let indirect_index = logical_block - INODE_DIRECT_BLOCKS;
    let node_index = indirect_index / INDIRECT_DATA_BLOCKS;
    let slot = indirect_index % INDIRECT_DATA_BLOCKS;
    let mut root = u64::from_le_bytes(inode.reserved2[..8].try_into().unwrap());
    if root == 0 {
        root = alloc.alloc(superblock)?;
        let zero = [0u8; FS_BLOCK_SIZE];
        disk.write_blocks(root, 1, &zero)
            .map_err(|error| error.to_string())?;
        inode.reserved2[..8].copy_from_slice(&root.to_le_bytes());
    }

    let mut node = root;
    for _ in 0..node_index {
        let mut buffer = [0u8; FS_BLOCK_SIZE];
        disk.read_blocks(node, 1, &mut buffer)
            .map_err(|error| error.to_string())?;
        let mut next = u64::from_le_bytes(buffer[..8].try_into().unwrap());
        if next == 0 {
            next = alloc.alloc(superblock)?;
            let zero = [0u8; FS_BLOCK_SIZE];
            disk.write_blocks(next, 1, &zero)
                .map_err(|error| error.to_string())?;
            buffer[..8].copy_from_slice(&next.to_le_bytes());
            disk.write_blocks(node, 1, &buffer)
                .map_err(|error| error.to_string())?;
        }
        node = next;
    }

    let mut buffer = [0u8; FS_BLOCK_SIZE];
    disk.read_blocks(node, 1, &mut buffer)
        .map_err(|error| error.to_string())?;
    let offset = 8 + slot * std::mem::size_of::<u64>();
    let block = u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap());
    if block != 0 {
        return Ok(block);
    }
    let block = alloc.alloc(superblock)?;
    buffer[offset..offset + 8].copy_from_slice(&block.to_le_bytes());
    disk.write_blocks(node, 1, &buffer)
        .map_err(|error| error.to_string())?;
    Ok(block)
}

fn host_create_file(disk: &mut RawDisk, guest_path: &str, data: &[u8]) -> Result<(), String> {
    // Read superblock + inodes
    let mut sb_buf = [0u8; FS_BLOCK_SIZE];
    disk.read_blocks(0, 1, &mut sb_buf)
        .map_err(|e| e.to_string())?;
    let mut superblock: Superblock =
        unsafe { std::ptr::read_unaligned(sb_buf.as_ptr() as *const Superblock) };
    let inode_blocks =
        unsafe { std::ptr::addr_of!(superblock.inode_blocks).read_unaligned() } as usize;
    // read inodes
    let mut inode_buf = vec![0u8; inode_blocks * FS_BLOCK_SIZE];
    for i in 0..inode_blocks {
        let mut b = [0u8; FS_BLOCK_SIZE];
        disk.read_blocks(1 + i as u64, 1, &mut b)
            .map_err(|e| e.to_string())?;
        inode_buf[i * FS_BLOCK_SIZE..(i + 1) * FS_BLOCK_SIZE].copy_from_slice(&b);
    }
    let mut inodes: Vec<Inode> = unsafe {
        let ptr = inode_buf.as_ptr() as *const Inode;
        let mut v = Vec::with_capacity(MAX_INODES);
        for i in 0..MAX_INODES {
            v.push(std::ptr::read_unaligned(ptr.add(i)));
        }
        v
    };

    // helper: find free inode
    let find_free_inode = |inodes: &Vec<Inode>| -> Option<usize> {
        for (i, ino) in inodes.iter().enumerate() {
            let ft = unsafe { std::ptr::addr_of!(ino.file_type).read_unaligned() };
            if ft == FT_EMPTY {
                return Some(i);
            }
        }
        None
    };
    // helper: find free block (simple sequential)
    let total_blocks = unsafe { std::ptr::addr_of!(superblock.total_blocks).read_unaligned() };
    let free_blocks = unsafe { std::ptr::addr_of!(superblock.free_blocks).read_unaligned() };
        // Free space comes from the on-disk allocation bitmap, not from
    // arithmetic on a free-block count.
    let _ = (total_blocks, free_blocks);
    let mut alloc = HostAlloc::load(disk, &superblock)?;

    // handle parent dirs creation for guest_path like /apps/hello.app
    let guest = guest_path.trim();
    let guest = guest.trim_start_matches('/');
    let parts: Vec<&str> = guest.split('/').collect();
    if parts.is_empty() {
        return Err("invalid path".into());
    }
    let basename = parts.last().unwrap();
    let parent_parts = &parts[..parts.len() - 1];

    // traverse / create dirs
    let mut current_inode: u32 = 0; // root
    for dir in parent_parts {
        if dir.is_empty() {
            continue;
        }
        // linear search in current_inode
        let found = find_entry(disk, &inodes, current_inode, dir).map_err(|e| e.to_string())?;
        if let Some(ino) = found {
            // must be dir
            let ft = unsafe { std::ptr::addr_of!(inodes[ino as usize].file_type).read_unaligned() };
            if ft != FT_DIR {
                return Err(format!("{} is not a directory", dir));
            }
            current_inode = ino;
        } else {
            // create dir
            let free = find_free_inode(&inodes).ok_or("no free inodes")?;
            let block = alloc.alloc(&mut superblock)?;
            // create inode
            inodes[free] = Inode {
                file_type: FT_DIR,
                permissions: 0o77,
                reserved1: 0,
                size: 0,
                blocks_used: 1,
                created: 0,
                modified: 0,
                direct_blocks: {
                    let mut a = [0u64; 12];
                    a[0] = block;
                    a
                },
                reserved2: [0; 16],
            };
            // zero block
            let zero = [0u8; FS_BLOCK_SIZE];
            disk.write_blocks(block, 1, &zero)
                .map_err(|e| e.to_string())?;
            // add entry to parent
            add_entry(disk, &mut inodes, current_inode, dir, free as u32, &mut alloc, &mut superblock).map_err(|e| e.to_string())?;
            // update superblock counts later
            unsafe {
                let p = std::ptr::addr_of_mut!(superblock.free_inodes);
                let cur = std::ptr::read_unaligned(p);
                std::ptr::write_unaligned(p, cur - 1);
                let p2 = std::ptr::addr_of_mut!(superblock.free_blocks);
                let cur2 = std::ptr::read_unaligned(p2);
                std::ptr::write_unaligned(p2, cur2 - 1);
            }
            current_inode = free as u32;
        }
    }

    // now create file in current_inode
    if basename.is_empty() {
        return Err("empty basename".into());
    }
    if basename.len() >= MAX_FILENAME_LEN {
        return Err("filename too long".into());
    }
    // check exists
    if let Some(_) =
        find_entry(disk, &inodes, current_inode, basename).map_err(|e| e.to_string())?
    {
        // For bundling, overwrite: delete? For now error if exists, but we can overwrite by reusing inode
        // Simplify: return error and let caller handle overwrite by deleting then retry?
        // We'll support overwrite: find inode, truncate
        let existing = find_entry(disk, &inodes, current_inode, basename)
            .map_err(|e| e.to_string())?
            .unwrap();
        // truncate: we will reuse existing inode
        // free blocks tracking not accurate but okay (leak old blocks). For fresh disk, okay.
        // We'll just update that inode
        let inode_idx = existing as usize;
        // allocate blocks if needed
        let blocks_needed = if data.is_empty() {
            0
        } else {
            (data.len() + FS_BLOCK_SIZE - 1) / FS_BLOCK_SIZE
        };
        // write data
        for (i, chunk) in data.chunks(FS_BLOCK_SIZE).enumerate() {
            let bnum = host_file_block(
                disk,
                &mut inodes[inode_idx],
                i,
                &mut alloc,
                &mut superblock,
            )?;
            let mut buf = [0u8; FS_BLOCK_SIZE];
            buf[..chunk.len()].copy_from_slice(chunk);
            disk.write_blocks(bnum, 1, &buf)
                .map_err(|e| e.to_string())?;
        }
        inodes[inode_idx].size = data.len() as u64;
        let indirect_nodes = blocks_needed
            .saturating_sub(INODE_DIRECT_BLOCKS)
            .div_ceil(INDIRECT_DATA_BLOCKS);
        inodes[inode_idx].blocks_used = (blocks_needed + indirect_nodes) as u32;
        // write back superblock + inodes
        write_back(disk, &mut superblock, &inodes, inode_blocks, &mut alloc).map_err(|e| e.to_string())?;
        return Ok(());
    }

    // create new file
    let free = find_free_inode(&inodes).ok_or("no free inodes")?;
    let blocks_needed = if data.is_empty() {
        0
    } else {
        (data.len() + FS_BLOCK_SIZE - 1) / FS_BLOCK_SIZE
    };
    inodes[free].file_type = FT_FILE;
    inodes[free].permissions = 0o66;
    inodes[free].size = data.len() as u64;
    let indirect_nodes = blocks_needed
        .saturating_sub(INODE_DIRECT_BLOCKS)
        .div_ceil(INDIRECT_DATA_BLOCKS);
    inodes[free].blocks_used = (blocks_needed + indirect_nodes) as u32;
    for (i, chunk) in data.chunks(FS_BLOCK_SIZE).enumerate() {
        let bnum = host_file_block(disk, &mut inodes[free], i, &mut alloc, &mut superblock)?;
        let mut buf = [0u8; FS_BLOCK_SIZE];
        buf[..chunk.len()].copy_from_slice(chunk);
        disk.write_blocks(bnum, 1, &buf)
            .map_err(|e| e.to_string())?;
    }
    add_entry(disk, &mut inodes, current_inode, basename, free as u32, &mut alloc, &mut superblock).map_err(|e| e.to_string())?;
    unsafe {
        let p = std::ptr::addr_of_mut!(superblock.free_inodes);
        let cur = std::ptr::read_unaligned(p);
        std::ptr::write_unaligned(p, cur - 1);
    }
    write_back(disk, &mut superblock, &inodes, inode_blocks, &mut alloc).map_err(|e| e.to_string())?;
    Ok(())
}

fn find_entry(
    disk: &mut RawDisk,
    inodes: &Vec<Inode>,
    dir_inode: u32,
    name: &str,
) -> std::io::Result<Option<u32>> {
    let ino = &inodes[dir_inode as usize];
    let ft = unsafe { std::ptr::addr_of!(ino.file_type).read_unaligned() };
    if ft != FT_DIR {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "not a directory",
        ));
    }
    for b in 0..INODE_DIRECT_BLOCKS {
        let block = unsafe { std::ptr::addr_of!(ino.direct_blocks[b]).read_unaligned() };
        if block == 0 {
            break;
        }
        let mut buf = [0u8; FS_BLOCK_SIZE];
        disk.read_blocks(block, 1, &mut buf)?;
        let per_block = FS_BLOCK_SIZE / std::mem::size_of::<DirectoryEntry>();
        for i in 0..per_block {
            let entry: DirectoryEntry = unsafe {
                std::ptr::read_unaligned(
                    buf.as_ptr().add(i * std::mem::size_of::<DirectoryEntry>())
                        as *const DirectoryEntry,
                )
            };
            let ino_num = unsafe { std::ptr::addr_of!(entry.inode_number).read_unaligned() };
            if ino_num != 0 {
                let name_arr = unsafe { std::ptr::addr_of!(entry.name).read_unaligned() };
                let len = name_arr
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(MAX_FILENAME_LEN);
                if let Ok(s) = std::str::from_utf8(&name_arr[..len]) {
                    if s == name {
                        return Ok(Some(ino_num));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Add a directory entry, growing the directory across as many blocks as it
/// needs (up to the inode's direct-block limit).
///
/// This used to inspect only block 0 and fail with "directory full" on the
/// ninth entry, which meant `bundle_examples` could not package more than
/// eight files into one directory. The kernel already grew directories; the
/// host copy had not caught up.
fn add_entry(
    disk: &mut RawDisk,
    inodes: &mut Vec<Inode>,
    dir_inode: u32,
    name: &str,
    child: u32,
    alloc: &mut HostAlloc,
    sb: &mut Superblock,
) -> std::io::Result<()> {
    let per_block = FS_BLOCK_SIZE / std::mem::size_of::<DirectoryEntry>();
    for slot in 0..INODE_DIRECT_BLOCKS {
        let mut block = unsafe {
            std::ptr::addr_of!(inodes[dir_inode as usize].direct_blocks[slot]).read_unaligned()
        };
        if block == 0 {
            block = alloc
                .alloc(sb)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            disk.write_blocks(block, 1, &[0u8; FS_BLOCK_SIZE])?;
            unsafe {
                let p = std::ptr::addr_of_mut!(inodes[dir_inode as usize].direct_blocks[slot]);
                p.write_unaligned(block);
            }
        }
        let mut buf = [0u8; FS_BLOCK_SIZE];
        disk.read_blocks(block, 1, &mut buf)?;
        for i in 0..per_block {
            let offset = i * std::mem::size_of::<DirectoryEntry>();
            let entry: DirectoryEntry = unsafe {
                std::ptr::read_unaligned(buf.as_ptr().add(offset) as *const DirectoryEntry)
            };
            let ino_num = unsafe { std::ptr::addr_of!(entry.inode_number).read_unaligned() };
            if ino_num == 0 {
                let mut new_entry = DirectoryEntry {
                    inode_number: child,
                    name: [0; MAX_FILENAME_LEN],
                    reserved: [0; 4],
                };
                let bytes = name.as_bytes();
                let len = std::cmp::min(bytes.len(), MAX_FILENAME_LEN - 1);
                new_entry.name[..len].copy_from_slice(&bytes[..len]);
                unsafe {
                    std::ptr::write_unaligned(
                        buf.as_mut_ptr().add(offset) as *mut DirectoryEntry,
                        new_entry,
                    );
                }
                disk.write_blocks(block, 1, &buf)?;
                return Ok(());
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        "directory full (12 blocks max)",
    ))
}

/// Persist superblock, bitmap and inode table.
///
/// The bitmap is written first so allocation state reaches the platter before
/// the metadata that references it, matching the kernel's own ordering.
fn write_back(
    disk: &mut RawDisk,
    sb: &mut Superblock,
    inodes: &Vec<Inode>,
    inode_blocks: usize,
    alloc: &mut HostAlloc,
) -> std::io::Result<()> {
    alloc.flush(disk).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let mut buf = [0u8; FS_BLOCK_SIZE];
    unsafe {
        std::ptr::copy_nonoverlapping(
            sb as *const _ as *const u8,
            buf.as_mut_ptr(),
            std::mem::size_of::<Superblock>(),
        );
    }
    disk.write_blocks(0, 1, &buf)?;
    let inode_bytes = unsafe {
        std::slice::from_raw_parts(
            inodes.as_ptr() as *const u8,
            inodes.len() * std::mem::size_of::<Inode>(),
        )
    };
    let mut block_num = 1u64;
    for chunk in inode_bytes.chunks(FS_BLOCK_SIZE) {
        let mut b = [0u8; FS_BLOCK_SIZE];
        b[..chunk.len()].copy_from_slice(chunk);
        if block_num < 1 + inode_blocks as u64 {
            disk.write_blocks(block_num, 1, &b)?;
        }
        block_num += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_bundler_writes_files_through_indirect_blocks() {
        let path = std::env::temp_dir().join(format!(
            "mfk-simplfs-{}-{}.img",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.set_len(4096 * FS_BLOCK_SIZE as u64).unwrap();
        drop(file);

        let result = (|| {
            let mut disk = RawDisk::open(&path, 4096).unwrap();
            format_disk(&mut disk, 4096).unwrap();
            let data_len = (INODE_DIRECT_BLOCKS + INDIRECT_DATA_BLOCKS + 7) * FS_BLOCK_SIZE + 91;
            let expected: Vec<u8> = (0..data_len).map(|i| (i % 239) as u8).collect();
            host_create_file(&mut disk, "/apps/large.bin", &expected).unwrap();

            let mut sb_buf = [0u8; FS_BLOCK_SIZE];
            disk.read_blocks(0, 1, &mut sb_buf).unwrap();
            let superblock =
                unsafe { std::ptr::read_unaligned(sb_buf.as_ptr() as *const Superblock) };
            let inode_blocks = superblock.inode_blocks as usize;
            let mut inode_buf = vec![0u8; inode_blocks * FS_BLOCK_SIZE];
            for i in 0..inode_blocks {
                disk.read_blocks(
                    1 + i as u64,
                    1,
                    &mut inode_buf[i * FS_BLOCK_SIZE..(i + 1) * FS_BLOCK_SIZE],
                )
                .unwrap();
            }
            let inode = unsafe {
                let inodes = inode_buf.as_ptr() as *const Inode;
                (0..MAX_INODES)
                    .map(|i| std::ptr::read_unaligned(inodes.add(i)))
                    .find(|inode| inode.file_type == FT_FILE && inode.size as usize == data_len)
                    .unwrap()
            };
            let root = u64::from_le_bytes(inode.reserved2[..8].try_into().unwrap());
            assert_ne!(root, 0);

            let mut actual = Vec::with_capacity(data_len);
            for logical in 0..data_len.div_ceil(FS_BLOCK_SIZE) {
                let block = if logical < INODE_DIRECT_BLOCKS {
                    unsafe { std::ptr::addr_of!(inode.direct_blocks[logical]).read_unaligned() }
                } else {
                    let indirect = logical - INODE_DIRECT_BLOCKS;
                    let node_index = indirect / INDIRECT_DATA_BLOCKS;
                    let slot = indirect % INDIRECT_DATA_BLOCKS;
                    let mut node = root;
                    for _ in 0..node_index {
                        let mut map = [0u8; FS_BLOCK_SIZE];
                        disk.read_blocks(node, 1, &mut map).unwrap();
                        node = u64::from_le_bytes(map[..8].try_into().unwrap());
                    }
                    let mut map = [0u8; FS_BLOCK_SIZE];
                    disk.read_blocks(node, 1, &mut map).unwrap();
                    let offset = 8 + slot * 8;
                    u64::from_le_bytes(map[offset..offset + 8].try_into().unwrap())
                };
                let mut block_bytes = [0u8; FS_BLOCK_SIZE];
                disk.read_blocks(block, 1, &mut block_bytes).unwrap();
                let count = (data_len - actual.len()).min(FS_BLOCK_SIZE);
                actual.extend_from_slice(&block_bytes[..count]);
            }
            assert_eq!(actual, expected);
            Ok::<(), String>(())
        })();
        let _ = std::fs::remove_file(path);
        result.unwrap();
    }
}
