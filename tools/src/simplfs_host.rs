//! Host-side SimplFS helper for bundling apps into raw disk.img
//! Minimal reimplementation of kernel/src/fs layout to run on std.
//! Only supports fresh format + file/dir creation for bundling.

use std::fs::File;
use std::io::{Read, Write, Seek, SeekFrom};
use std::path::Path;

const FS_BLOCK_SIZE: usize = 512;
const MAX_FILENAME_LEN: usize = 56;
const MAX_INODES: usize = 256;
const INODE_DIRECT_BLOCKS: usize = 12;

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
    reserved: [u8; 456],
}
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
const FT_EMPTY: u8 = 0;
const FT_FILE: u8 = 1;
const FT_DIR: u8 = 2;

fn inode_size() -> usize { std::mem::size_of::<Inode>() }

struct RawDisk {
    file: File,
    block_count: u64,
}

impl RawDisk {
    fn open(path: &Path, block_count: u64) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file, block_count })
    }
    fn read_blocks(&mut self, start: u64, count: usize, buf: &mut [u8]) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(start * FS_BLOCK_SIZE as u64))?;
        let to_read = count * FS_BLOCK_SIZE;
        assert!(buf.len() >= to_read);
        self.file.read_exact(&mut buf[..to_read])?;
        Ok(())
    }
    fn write_blocks(&mut self, start: u64, count: usize, buf: &[u8]) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(start * FS_BLOCK_SIZE as u64))?;
        let to_write = count * FS_BLOCK_SIZE;
        assert!(buf.len() >= to_write);
        self.file.write_all(&buf[..to_write])?;
        self.file.flush()?;
        Ok(())
    }
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
    disk.read_blocks(0, 1, &mut sb_buf).map_err(|e| e.to_string())?;
    let magic = u32::from_le_bytes([sb_buf[0], sb_buf[1], sb_buf[2], sb_buf[3]]);
    let is_formatted = magic == SUPER_MAGIC;

    if !is_formatted {
        println!("Host bundle: disk not formatted, formatting SimplFS ({} blocks)...", total_blocks);
        format_disk(&mut disk, total_blocks).map_err(|e| e.to_string())?;
    } else {
        // verify we can mount (read inode table) - if fails, reformat?
        println!("Host bundle: disk already formatted (magic ok), injecting files...");
    }

    // need to perform file creation using our host FS logic (re-read superblock, inodes, etc.)
    // For simplicity after formatting, we will create dirs and files
    for (guest_path, host_path) in &files {
        let data = std::fs::read(host_path).map_err(|e| e.to_string())?;
        println!("  bundling {} -> {} ({} bytes)", host_path, guest_path, data.len());
        host_create_file(&mut disk, guest_path, &data).map_err(|e| format!("{}: {}", guest_path, e))?;
    }

    println!("Host bundle: injected {} file(s) into {}", files.len(), disk_path.display());
    println!("  Inside kernel: mount; ls /apps; run /apps/hello.app");
    Ok(())
}

fn collect_files(root: &Path, cur: &Path, out: &mut Vec<(String,String)>) {
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
    let data_start = 1 + inode_blocks as u64;
    let superblock = Superblock {
        magic: SUPER_MAGIC,
        version: 1,
        block_size: FS_BLOCK_SIZE as u32,
        total_blocks,
        inode_count: MAX_INODES as u32,
        inode_blocks: inode_blocks as u32,
        data_block_start: data_start,
        free_blocks: total_blocks - data_start - 1, // minus root dir block
        free_inodes: (MAX_INODES as u32) - 1,
        root_inode: 0,
        reserved: [0;456],
    };
    // write superblock
    let mut buf = [0u8; FS_BLOCK_SIZE];
    unsafe {
        let src = &superblock as *const _ as *const u8;
        std::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), std::mem::size_of::<Superblock>());
    }
    // Ensure LE (host is little endian, same as kernel)
    disk.write_blocks(0, 1, &buf)?;

    // inode table
    let mut inodes = vec![Inode {
        file_type: FT_EMPTY,
        permissions: 0,
        reserved1: 0,
        size: 0,
        blocks_used: 0,
        created: 0,
        modified: 0,
        direct_blocks: [0; INODE_DIRECT_BLOCKS],
        reserved2: [0;16],
    }; MAX_INODES];
    inodes[0].file_type = FT_DIR;
    inodes[0].permissions = 0o77;
    inodes[0].blocks_used = 1;
    inodes[0].direct_blocks[0] = data_start;

    let inode_bytes = unsafe {
        std::slice::from_raw_parts(inodes.as_ptr() as *const u8, inodes.len()* inode_size())
    };
    let mut block_num = 1u64;
    for chunk in inode_bytes.chunks(FS_BLOCK_SIZE) {
        let mut b = [0u8; FS_BLOCK_SIZE];
        b[..chunk.len()].copy_from_slice(chunk);
        disk.write_blocks(block_num, 1, &b)?;
        block_num+=1;
    }
    // clear root dir block
    let zero = [0u8; FS_BLOCK_SIZE];
    disk.write_blocks(data_start, 1, &zero)?;
    Ok(())
}

fn host_create_file(disk: &mut RawDisk, guest_path: &str, data: &[u8]) -> Result<(), String> {
    // Read superblock + inodes
    let mut sb_buf = [0u8; FS_BLOCK_SIZE];
    disk.read_blocks(0,1,&mut sb_buf).map_err(|e| e.to_string())?;
    let mut superblock: Superblock = unsafe { std::ptr::read_unaligned(sb_buf.as_ptr() as *const Superblock) };
    let inode_blocks = unsafe { std::ptr::addr_of!(superblock.inode_blocks).read_unaligned() } as usize;
    let data_start = unsafe { std::ptr::addr_of!(superblock.data_block_start).read_unaligned() };

    // read inodes
    let mut inode_buf = vec![0u8; inode_blocks * FS_BLOCK_SIZE];
    for i in 0..inode_blocks {
        let mut b = [0u8; FS_BLOCK_SIZE];
        disk.read_blocks(1 + i as u64, 1, &mut b).map_err(|e| e.to_string())?;
        inode_buf[i*FS_BLOCK_SIZE .. (i+1)*FS_BLOCK_SIZE].copy_from_slice(&b);
    }
    let mut inodes: Vec<Inode> = unsafe {
        let ptr = inode_buf.as_ptr() as *const Inode;
        let mut v = Vec::with_capacity(MAX_INODES);
        for i in 0..MAX_INODES { v.push(std::ptr::read_unaligned(ptr.add(i))); }
        v
    };

    // helper: find free inode
    let find_free_inode = |inodes: &Vec<Inode>| -> Option<usize> {
        for (i, ino) in inodes.iter().enumerate() {
            let ft = unsafe { std::ptr::addr_of!(ino.file_type).read_unaligned() };
            if ft == FT_EMPTY { return Some(i); }
        }
        None
    };
    // helper: find free block (simple sequential)
    let total_blocks = unsafe { std::ptr::addr_of!(superblock.total_blocks).read_unaligned() };
    let free_blocks = unsafe { std::ptr::addr_of!(superblock.free_blocks).read_unaligned() };
    let mut next_block = data_start + (total_blocks - data_start - free_blocks);

    // handle parent dirs creation for guest_path like /apps/hello.app
    let guest = guest_path.trim();
    let guest = guest.trim_start_matches('/');
    let parts: Vec<&str> = guest.split('/').collect();
    if parts.is_empty() { return Err("invalid path".into()); }
    let basename = parts.last().unwrap();
    let parent_parts = &parts[..parts.len()-1];

    // traverse / create dirs
    let mut current_inode: u32 = 0; // root
    for dir in parent_parts {
        if dir.is_empty() { continue; }
        // linear search in current_inode
        let found = find_entry(disk, &inodes, current_inode, dir).map_err(|e| e.to_string())?;
        if let Some(ino) = found {
            // must be dir
            let ft = unsafe { std::ptr::addr_of!(inodes[ino as usize].file_type).read_unaligned() };
            if ft != FT_DIR { return Err(format!("{} is not a directory", dir)); }
            current_inode = ino;
        } else {
            // create dir
            let free = find_free_inode(&inodes).ok_or("no free inodes")?;
            let block = next_block;
            next_block += 1;
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
                    let mut a=[0u64;12]; a[0]=block; a
                },
                reserved2: [0;16],
            };
            // zero block
            let zero = [0u8; FS_BLOCK_SIZE];
            disk.write_blocks(block,1,&zero).map_err(|e| e.to_string())?;
            // add entry to parent
            add_entry(disk, &inodes, current_inode, dir, free as u32).map_err(|e| e.to_string())?;
            // update superblock counts later
            unsafe {
                let p = std::ptr::addr_of_mut!(superblock.free_inodes);
                let cur = std::ptr::read_unaligned(p);
                std::ptr::write_unaligned(p, cur -1);
                let p2 = std::ptr::addr_of_mut!(superblock.free_blocks);
                let cur2 = std::ptr::read_unaligned(p2);
                std::ptr::write_unaligned(p2, cur2 -1);
            }
            current_inode = free as u32;
        }
    }

    // now create file in current_inode
    if basename.is_empty() { return Err("empty basename".into());}
    if basename.len() >= MAX_FILENAME_LEN { return Err("filename too long".into());}
    // check exists
    if let Some(_) = find_entry(disk, &inodes, current_inode, basename).map_err(|e| e.to_string())? {
        // For bundling, overwrite: delete? For now error if exists, but we can overwrite by reusing inode
        // Simplify: return error and let caller handle overwrite by deleting then retry?
        // We'll support overwrite: find inode, truncate
        let existing = find_entry(disk, &inodes, current_inode, basename).map_err(|e| e.to_string())?.unwrap();
        // truncate: we will reuse existing inode
        // free blocks tracking not accurate but okay (leak old blocks). For fresh disk, okay.
        // We'll just update that inode
        let inode_idx = existing as usize;
        // allocate blocks if needed
        let blocks_needed = if data.is_empty() {0} else {(data.len()+FS_BLOCK_SIZE-1)/FS_BLOCK_SIZE};
        if blocks_needed > INODE_DIRECT_BLOCKS { return Err("file too large".into());}
        // Allocate blocks for those that are 0
        for i in 0..blocks_needed {
            let cur = unsafe { std::ptr::addr_of!(inodes[inode_idx].direct_blocks[i]).read_unaligned() };
            if cur==0 {
                let nb = next_block; next_block+=1;
                inodes[inode_idx].direct_blocks[i]=nb;
                unsafe {
                    let p2 = std::ptr::addr_of_mut!(superblock.free_blocks);
                    let cur2 = std::ptr::read_unaligned(p2);
                    std::ptr::write_unaligned(p2, cur2 -1);
                }
            }
        }
        // write data
        for (i, chunk) in data.chunks(FS_BLOCK_SIZE).enumerate() {
            let bnum = unsafe { std::ptr::addr_of!(inodes[inode_idx].direct_blocks[i]).read_unaligned() };
            let mut buf=[0u8; FS_BLOCK_SIZE];
            buf[..chunk.len()].copy_from_slice(chunk);
            disk.write_blocks(bnum,1,&buf).map_err(|e| e.to_string())?;
        }
        inodes[inode_idx].size = data.len() as u64;
        inodes[inode_idx].blocks_used = blocks_needed as u32;
        // write back superblock + inodes
        write_back(disk, &superblock, &inodes, inode_blocks).map_err(|e| e.to_string())?;
        return Ok(());
    }

    // create new file
    let free = find_free_inode(&inodes).ok_or("no free inodes")?;
    let blocks_needed = if data.is_empty() {0} else {(data.len()+FS_BLOCK_SIZE-1)/FS_BLOCK_SIZE};
    if blocks_needed > INODE_DIRECT_BLOCKS { return Err("file too large".into());}
    inodes[free].file_type = FT_FILE;
    inodes[free].permissions = 0o66;
    inodes[free].size = data.len() as u64;
    inodes[free].blocks_used = blocks_needed as u32;
    for i in 0..blocks_needed {
        let nb = next_block; next_block+=1;
        inodes[free].direct_blocks[i]=nb;
        unsafe {
            let p2 = std::ptr::addr_of_mut!(superblock.free_blocks);
            let cur2 = std::ptr::read_unaligned(p2);
            std::ptr::write_unaligned(p2, cur2 -1);
        }
    }
    for (i, chunk) in data.chunks(FS_BLOCK_SIZE).enumerate() {
        let bnum = inodes[free].direct_blocks[i];
        let mut buf=[0u8; FS_BLOCK_SIZE];
        buf[..chunk.len()].copy_from_slice(chunk);
        disk.write_blocks(bnum,1,&buf).map_err(|e| e.to_string())?;
    }
    add_entry(disk, &inodes, current_inode, basename, free as u32).map_err(|e| e.to_string())?;
    unsafe {
        let p = std::ptr::addr_of_mut!(superblock.free_inodes);
        let cur = std::ptr::read_unaligned(p);
        std::ptr::write_unaligned(p, cur -1);
    }
    write_back(disk, &superblock, &inodes, inode_blocks).map_err(|e| e.to_string())?;
    Ok(())
}

fn find_entry(disk: &mut RawDisk, inodes: &Vec<Inode>, dir_inode: u32, name: &str) -> std::io::Result<Option<u32>> {
    let ino = &inodes[dir_inode as usize];
    let ft = unsafe { std::ptr::addr_of!(ino.file_type).read_unaligned() };
    if ft != FT_DIR { return Err(std::io::Error::new(std::io::ErrorKind::Other, "not a directory")); }
    for b in 0..INODE_DIRECT_BLOCKS {
        let block = unsafe { std::ptr::addr_of!(ino.direct_blocks[b]).read_unaligned() };
        if block==0 { break; }
        let mut buf=[0u8; FS_BLOCK_SIZE];
        disk.read_blocks(block,1,&mut buf)?;
        let per_block = FS_BLOCK_SIZE / std::mem::size_of::<DirectoryEntry>();
        for i in 0..per_block {
            let entry: DirectoryEntry = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(i*std::mem::size_of::<DirectoryEntry>()) as *const DirectoryEntry) };
            let ino_num = unsafe { std::ptr::addr_of!(entry.inode_number).read_unaligned() };
            if ino_num !=0 {
                let name_arr = unsafe { std::ptr::addr_of!(entry.name).read_unaligned() };
                let len = name_arr.iter().position(|&c| c==0).unwrap_or(MAX_FILENAME_LEN);
                if let Ok(s)=std::str::from_utf8(&name_arr[..len]) {
                    if s==name { return Ok(Some(ino_num)); }
                }
            }
        }
    }
    Ok(None)
}

fn add_entry(disk: &mut RawDisk, inodes: &Vec<Inode>, dir_inode: u32, name: &str, child: u32) -> std::io::Result<()> {
    let ino = &inodes[dir_inode as usize];
    let block = unsafe { std::ptr::addr_of!(ino.direct_blocks[0]).read_unaligned() };
    if block==0 { return Err(std::io::Error::new(std::io::ErrorKind::Other, "dir no blocks")); }
    let mut buf=[0u8; FS_BLOCK_SIZE];
    disk.read_blocks(block,1,&mut buf)?;
    let per_block = FS_BLOCK_SIZE / std::mem::size_of::<DirectoryEntry>();
    for i in 0..per_block {
        let offset = i*std::mem::size_of::<DirectoryEntry>();
        let entry: DirectoryEntry = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(offset) as *const DirectoryEntry) };
        let ino_num = unsafe { std::ptr::addr_of!(entry.inode_number).read_unaligned() };
        if ino_num==0 {
            let mut new_entry = DirectoryEntry {
                inode_number: child,
                name: [0; MAX_FILENAME_LEN],
                reserved: [0;4],
            };
            let bytes=name.as_bytes();
            let len=std::cmp::min(bytes.len(), MAX_FILENAME_LEN-1);
            new_entry.name[..len].copy_from_slice(&bytes[..len]);
            unsafe { std::ptr::write_unaligned(buf.as_mut_ptr().add(offset) as *mut DirectoryEntry, new_entry); }
            disk.write_blocks(block,1,&buf)?;
            return Ok(());
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::Other, "directory full"))
}

fn write_back(disk: &mut RawDisk, sb: &Superblock, inodes: &Vec<Inode>, inode_blocks: usize) -> std::io::Result<()> {
    let mut buf=[0u8; FS_BLOCK_SIZE];
    unsafe { std::ptr::copy_nonoverlapping(sb as *const _ as *const u8, buf.as_mut_ptr(), std::mem::size_of::<Superblock>()); }
    disk.write_blocks(0,1,&buf)?;
    let inode_bytes = unsafe { std::slice::from_raw_parts(inodes.as_ptr() as *const u8, inodes.len()* std::mem::size_of::<Inode>()) };
    let mut block_num=1u64;
    for chunk in inode_bytes.chunks(FS_BLOCK_SIZE) {
        let mut b=[0u8; FS_BLOCK_SIZE];
        b[..chunk.len()].copy_from_slice(chunk);
        if block_num < 1 + inode_blocks as u64 {
            disk.write_blocks(block_num,1,&b)?;
        }
        block_num+=1;
    }
    Ok(())
}
