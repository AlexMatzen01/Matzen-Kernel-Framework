# Filesystem (SimplFS)

Reference for MFK's filesystem: on-disk layout, the guarantees it provides,
and how to check them.

## On-disk layout

512-byte blocks throughout.

```text
block 0                        superblock (exactly 512 bytes)
block 1    .. inode_blocks     inode table   (72 blocks: 256 inodes x 144 B)
block bitmap_start ..          allocation bitmap
block data_block_start ..      data blocks   (file data, directory blocks, indirect nodes)
```

| Constant | Value | Note |
|---|---|---|
| `FS_BLOCK_SIZE` | 512 | |
| `MAX_FILENAME_LEN` | 56 | 55 usable bytes, NUL-terminated |
| `MAX_INODES` | 256 | |
| `INODE_DIRECT_BLOCKS` | 12 | Per inode |
| `INDIRECT_DATA_BLOCKS` | 63 | Per indirect node |
| Version | 2 | |

The bitmap's position is *derived*, not stored twice: it starts immediately
after the inode table, so `bitmap_start == 1 + inode_blocks` is a validated
invariant rather than a second source of truth.

### Superblock

```text
 0  magic          u32   "SFMK" (0x53464D4B)
 4  version        u32   2
 8  block_size     u32   512
12  total_blocks   u64
20  inode_count    u32   <= MAX_INODES, validated at mount
24  inode_blocks   u32
28  data_block_start u64
36  free_blocks    u64   must agree with the bitmap
44  free_inodes    u32
48  root_inode     u32
52  bitmap_start   u64   == 1 + inode_blocks
60  bitmap_blocks  u32
64  reserved       [u8; 448]
```

The struct is exactly one block; a `const` assertion enforces it. The previous
layout summed to 508 bytes, leaving four bytes of block 0 unversioned and
undocumented.

### Allocation bitmap (version 2)

One bit per data block, indexed relative to `data_block_start`, persisted to
disk and written back before the metadata that references it.

This is the central correctness change. Version 1 kept free-space state **only
in RAM** and rebuilt it at mount by walking every inode. That walk cannot
distinguish "block allocated, inode record never written" from "block free", so
any interrupted operation caused a live block to be handed out a second time —
silent data corruption. Version 1 images are refused rather than mounted.

The bitmap is written back per *bitmap block*, not per data block, so one
allocation dirties one page rather than the whole map.

### Indirect blocks

File data is addressed as 12 direct blocks followed by a chain of 63-entry
indirect nodes. Node layout:

```text
[0..8]   LBA of the next node (0 = end)
[8..512] 63 x u64 data block LBAs
```

The chain head is stored in `Inode.reserved2[..8]`.

Known limitations (unchanged in version 2):

- Indirection depth is unbounded, paid as a linear chain walk. Random access to
  block *n* costs `ceil(n/63)` reads in the worst case; a single-entry write-back
  cursor (`ChainCursor`) makes sequential access linear.
- Holes cannot be expressed explicitly: a missing entry reads as block 0, i.e.
  "not mapped".

### Directories

A directory is a run of 64-byte entries, eight per block, growing across up to
12 blocks:

```text
 0  inode_number  u32   0 = free slot
 4  name          [u8; 56]
60  reserved      [u8; 4]
```

A directory's `size` records the extent of its non-empty block list, so readers
can bound their scan and `blocks_used` is meaningful.

Version 2 removes the single-block restriction: `add_entry_to_dir` previously
inspected only block 0 and returned `"Directory is full"` on the ninth entry,
even though every reader already walked all twelve. That made a directory
listable but not writable, and it broke the host bundler, which failed outright
on more than eight files in one directory.

There are no `.` or `..` entries. The kernel keeps an in-memory parent map
instead, rebuilt from disk at mount and maintained by `create_file`,
`create_directory`, `delete_file` and `rename_file`.

## Guarantees and non-guarantees

Provided:

- Free-space state is persisted, so no block is handed out twice across a
  remount.
- Block pointers and directory entries are validated before use, so a corrupt
  image yields an error rather than a kernel panic.
- Every inode reference lies within the inode table; every block reference
  within the data region.
- Free-block allocation never returns the root directory's block.

Not provided:

| Capability | Status |
|---|---|
| Journaling / atomicity | **Absent.** A crash can leave a dangling directory entry or leaked blocks. |
| Timestamps | Fields exist but are never written; `tar` extraction writes zero mtimes. |
| Permissions | Written once, never enforced. No uid/gid, no ACL. |
| Symbolic / hard links | Absent. |
| Sparse files | Half-implemented: `read_file_range` zero-fills a hole, `read_file` errors on the same state. |
| Locking, quotas, snapshots, encryption | Absent. |
| Multi-filesystem mounts | One volume at a time; no mount table, no unmount. |

Version 2 makes free-space accounting durable but does **not** add
crash-atomicity: the ordering within each operation is still unordered, so an
interrupted metadata update can leave a dangling entry. See the "Future"
section below.

## Checking consistency

`fsck` is read-only and reports seven classes of damage. It is the only
meaningful integrity check: with version 1's inode-derived free space, the
bitmap and the inode table agreed by construction and nothing could be
detected.

```
Geometry        superblock fields that contradict each other
Block counts    free_blocks disagreeing with the bitmap
Block pointers  block references outside the data region
Indirect chains chains that do not terminate (cycles)
Dir entries     entries naming a non-existent or out-of-range inode
Space refs      blocks an inode references that the bitmap calls free
Leaked space    blocks marked used that no inode references
```

## Host tooling

`tools/src/simplfs_host.rs` builds disk images containing bundled apps and
assets. It shares no code with the kernel's filesystem — the structs are
duplicated by hand — so the two are tied together by:

- `const _: () = assert!(std::mem::size_of::<Superblock>() == FS_BLOCK_SIZE);`
  and the same for `Inode` (144) and `DirectoryEntry` (64), so a layout change
  fails to compile rather than producing an unmountable image.
- The host reads the same on-disk allocation bitmap and allocates from it,
  instead of inferring the next free block arithmetically from a free-block
  count. That inference assumed used blocks formed one contiguous run and
  diverged from the kernel's bitmap the moment a block in the middle of the
  region was freed.
- The host validates `superblock.version` and reformats a version 1 image
  rather than appending to it, since the kernel would refuse to mount it.

Format changes must be mirrored in both places.

## Source map

| File | Contents |
|---|---|
| `kernel/src/fs/mod.rs` | SimplFS: layout, inode table, directories, bitmap, read/write paths |
| `kernel/src/memory/frame_allocator.rs` | Physical frames the filesystem allocates metadata from |
| `tools/src/simplfs_host.rs` | Host-side image builder for `--bundle-apps` / `--doom-wad` |
