# Memory Management

Reference for MFK's memory subsystem: how physical memory is discovered and
classified, how frames and heap bytes are handed out, and what the `mem`
command reports.

## Address space

MFK runs in ring 0 only and does not implement its own virtual memory. The
bootloader establishes the mappings and the kernel reaches RAM through a
**physical direct map**: a physical address `p` is a valid pointer at
`physical_memory_offset + p`. The offset is chosen by the bootloader and is
printed at boot (`Physical memory offset: 0x...`) and reported by `mem`.

| Region | How it is reached |
|---|---|
| Kernel code/data | Bootloader's own mapping |
| Kernel stack | Bootloader's own mapping (80 KiB default) |
| Heap | Direct map at `offset + heap_phys` |
| VGA text buffer | Direct map (`0xB8000`) |
| Physical frames | Direct map |
| PCI MMIO | Direct map when below the mapped range, otherwise fresh page tables via `drivers::pci` |

`crate::memory::addr` owns that arithmetic. Drivers must not re-derive it:
`addr::dma_phys` also confirms a 32-bit DMA controller can reach the address.

### Direct-map range

The bootloader maps `[0, max(highest region end, 4 GiB))` — at least 4 GiB, so
MMIO regions stay reachable on a machine with less RAM than that. That 4 GiB
floor matters: treating the mapped range as ending at the highest region end
makes every sub-4 GiB PCI BAR look unmapped on a small guest, so each one takes
the page-table path and leaks page-table frames for a mapping that already
exists. `drivers::pci::PHYS_MAP_END` therefore holds the floored value.

## Firmware memory map

`crate::memory::memmap` interprets the firmware map once, in `kernel_main`:

1. **Classify** every region. The raw tags are `Usable`, `Bootloader`, or an
   `Unknown*` variant carrying the firmware type (UEFI `MemoryType` or BIOS
   E820 type). Each maps to a `RegionClass`:

   | Class | Policy |
   |---|---|
   | `usable` | Allocatable |
   | `acpi-reclaim` | Allocatable (the OS claims it) |
   | `bootloader` | Mapped, never allocated |
   | `acpi-nvs` | Must survive reboots — never allocated |
   | `bad` | Never allocated |
   | `mmio` | Never allocated |
   | `reserved` | Never allocated |

   Any firmware type the kernel does not recognise maps to `reserved`. An
   unidentified region must never become free RAM.

2. **Reserve** the spans the kernel itself occupies. The bootloader marks its
   own structures, but not everything: the kernel ELF (from
   `BootInfo::kernel_addr`/`kernel_len`), a ramdisk, the linear framebuffer,
   and the heap are recorded explicitly.

3. **Produce spans** — page-aligned, reservation-free, adjacent spans merged.
   This is what the frame allocator is given, and it is computed over *every*
   region, not the capped display list.

`MAX_REGIONS` (64) caps only how many regions `mem` prints; the byte totals
stay exact, and `allocatable_spans` is unaffected by the cap.

### Why "total" is not "RAM"

A firmware map routinely contains a PCI MMIO hole covering most of the upper
address space. Counting it as physical memory makes `mem` claim far more RAM
than the machine has, so `mem` leads with the **allocatable** figure and prints
the per-class breakdown beneath it, which reconciles the two.

## Frame allocator

`crate::memory::frame_allocator` is bitmap-backed, with counters and free.

| Property | Value |
|---|---|
| Granularity | 4 KiB, plus naturally aligned 2 MiB frames |
| Allocation | First fit from a rotating per-region cursor, wrapping |
| Free | Yes — `FrameDeallocator` for 4 KiB |
| Persistence | None: frames are a kernel-lifetime resource, not on disk |
| DMA pool | `allocate_dma_frame` returns only frames below 4 GiB |
| Bookkeeping | `FrameStats`: totals, free, used, low/high, ops, failures |
| Bitmap budget | 4 MiB, tracking 32 GiB of RAM; overflow is skipped and logged |

4 GiB is the DMA ceiling because every bus master in this kernel (EHCI, UHCI,
OHCI, xHCI, legacy virtio-blk, PIO-mode IDE) addresses memory with a 32-bit
physical pointer. The limit lives in `memmap::DMA_PHYS_LIMIT` and every driver
shares it.

A 2 MiB frame must start on a 2 MiB boundary, so the unaligned head of a
region is skipped rather than producing a misaligned "huge" frame.

The previous implementation was a bump cursor: it could never return a frame,
kept no counters, and — because it walked regions in ascending order with no
upper bound — would eventually hand out frames above 4 GiB that the DMA
controllers cannot address, turning into silent driver failure.

## Kernel heap

The heap is a `TrackingAllocator` wrapping `linked_list_allocator::LockedHeap`.

```text
  base                                                  base + size
  |------ main heap (HEAP_SIZE, 32 MiB) ------|-- reserve (1 MiB) --|
     first-fit hole list, freed normally           bump cursor, one-shot
```

It must not live in `.bss`: a 32 MiB static array balloons the kernel ELF and
collides with bootloader mappings on real hardware. It is carved from the
firmware map instead, from the **tail of the largest usable run**, preferring
memory below 4 GiB.

The default carve *refuses* a run above 4 GiB rather than falling back to one.
Every DMA buffer comes from this heap, so a heap above 4 GiB would leave USB
and virtio-blk unable to address any of it — they would fail silently.

If no sub-4 GiB run exists, the kernel boots on a 1 MiB static fallback with a
serial warning.

### Instrumentation

| Field | Meaning |
|---|---|
| `size` / `used` / `free` | From the hole list |
| `peak_bytes` | High-water mark of live allocated bytes |
| `peak_single` | Largest single allocation ever served |
| `live_allocations` | Outstanding allocations |
| `alloc_requests` | Total `alloc`/`realloc` calls |
| `total_allocated` / `total_freed` | Lifetime byte totals |
| `oom_events` | Heap-exhaustion events |
| `reserve_used` | Emergency-reserve bytes handed out |

### Out-of-memory behaviour

An exhausted heap routes to an explicit `#[alloc_error_handler]` that formats
only integers, reports over serial, and halts. It deliberately does **not**
panic: the panic handler formats a `PanicInfo`, which allocates, so a
heap-exhaustion panic could recurse into the allocator it had already failed.

`allocator::try_alloc_emergency` allocates from the reserve for paths that must
survive low memory. Reserve allocations are never reused.

## Physical addressing width

`sysinfo::cpu_max_phys_addr_bits()` reads `CPUID.0x80000008:EAX[7:0]`
(`MAXPHYADDR`), adding 32 when the extension bit is set. Without it nothing in
the kernel knew how wide physical memory is, so a 64-bit PCI BAR above that
width could not be detected as unreachable. `cpu_phys_addr_limit()` is the
value to use for range checks, defaulting to the x86-64 baseline of 40 bits.

## SMBIOS

`crate::drivers::smbios` reports what the RAM physically *is* — module sizes,
form factor, ECC, manufacturer part numbers — which the memory map cannot say.

Types 16 (Physical Memory Array) and 17 (Physical Memory Device) are decoded
from a bounds-checked parser over a byte slice. Two details the format forces:

- **String handles are single bytes.** A type 17 record's string fields are
  8-bit indices into the record's string pool, not 16-bit.
- **Type 16 has two layouts.** SMBIOS 2.x (length `0x0F`) and 3.x (length
  `>= 0x17`) place the handle, slot count, capacity and ECC bytes at different
  offsets. The record's own length byte selects the layout; a length matching
  neither is not decoded, because a plausible-looking wrong handle is worse
  than no array at all.

Entry-point discovery scans the RSDP page, the BIOS data area, and the BIOS ROM
area for `SMBIOS3` / `_SM_` rather than trusting a fixed RSDP offset.

Firmware is not obliged to expose a guest-reachable entry point — **OVMF is
not** — so `mem` reports "not exposed by firmware" on a plain UEFI guest. The
memory map and CPUID remain authoritative in that case.

## `mem`

```
Memory Information:
  Allocatable:  455 MiB after kernel-owned reservations
  Firmware map: 12.5 GiB described across 64 region(s) (list truncated)
    usable            486 MiB  3.8%
    reserved         12.0 GiB  96.0%
    bootloader       22.9 MiB  0.2%
    acpi-reclaim     2.04 MiB  0.0%
    bad              72.0 KiB  0.0%
  DMA reach:    488 MiB below 4 GiB, 0 B above (32-bit controllers)
...
```

Every number is read from live state. The command previously printed a fixed
1994-era 640 KB/384 KB/ROM map that matched no real machine.

## Source map

| File | Contents |
|---|---|
| `memory/memmap.rs` | Region classification, reservations, spans, per-class totals |
| `memory/frame_allocator.rs` | Bitmap frame allocator, DMA pool, 2 MiB frames, stats |
| `memory/addr.rs` | Direct-map translation and DMA-reachability checks |
| `allocator.rs` | Heap, tracking allocator, OOM handler, emergency reserve |
| `sysinfo.rs` | Layout snapshot, CPUID, address width, region naming |
| `drivers/smbios.rs` | SMBIOS types 16/17 parsing and entry-point discovery |
