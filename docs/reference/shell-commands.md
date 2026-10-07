# Shell Commands Reference

Complete list of all built-in shell commands.

## Help & Information

### `help`
Display all available commands with brief descriptions.

```bash
mfk> help
Available commands:
  help      - Display this help message
  ...
```

### `about`
Show project information and features.

```bash
mfk> about
Matzen Kernel Framework (MFK) v0.1.0
A simple terminal OS written in Rust

Features:
  - VGA text mode output
  - PS/2 keyboard input
  - Built-in command shell
  ...
```

### `version`
Display kernel version and build info.

```bash
mfk> version
Matzen Kernel Framework (MFK)
  Version:  0.1.0
  Build:    debug
  Arch:     x86_64
  Compiler: rustc (nightly)
```

### `cpuinfo`
Display CPU name, vendor, signature, topology, and features.

```bash
mfk> cpuinfo
CPU Information:
  Name: Intel(R) Core(TM) i7-...
  Vendor: GenuineIntel
  Family: 6, Model: 142, Stepping: 10
  Physical cores: 8
  Logical threads: 16
  Features: FPU ...
```

## Display & Terminal

### `clear` / `cls`
Clear the terminal screen.

```bash
mfk> clear
```

Clears VGA buffer and positions cursor at top-left.

### `echo <text>`
Print text to the terminal.

```bash
mfk> echo "Hello, MFK!"
Hello, MFK!

mfk> echo "Current system: $(uptime)"
```

### `color <color>`
Change terminal text color.

```bash
mfk> color green      # Green text
mfk> color white      # White text (default)
mfk> color cyan       # Cyan text
mfk> color yellow     # Yellow text
mfk> color red        # Red text
mfk> color blue       # Blue text
mfk> color pink       # Pink/Magenta text
```

## System Information

### `uptime`
Show system uptime since boot.

```bash
mfk> uptime
System uptime: 5 minutes, 23 seconds
(Loop iterations: 52340000)
```

Note: Approximation based on main loop iterations (~1000/sec).

### `memory` / `mem`
Display memory accounting read from live state: the interpreted firmware map,
heap usage, frame allocator counters, and the SMBIOS inventory if the firmware
exposes one.

```bash
mfk> mem
Memory Information:
  Allocatable:  455 MiB after kernel-owned reservations
  Firmware map: 12.5 GiB described across 64 region(s) (list truncated)
    usable            486 MiB  3.8%
    reserved         12.0 GiB  96.0%
    bootloader       22.9 MiB  0.2%
    acpi-reclaim     2.04 MiB  0.0%
    bad              72.0 KiB  0.0%
  DMA reach:    488 MiB below 4 GiB, 0 B above (32-bit controllers)

Kernel Heap:
  Carve:       32.0 MiB at physical 0x28019e6d000
  Used/Free:   4.01 MiB / 28.0 MiB (12.5% used)
  Peak:        4.01 MiB live, largest single 3.91 MiB
  Requests:    80 total, 59 outstanding
  Reserve:     0 B used of 1.00 MiB (OOM-time allocations)
  OOM events:  0

Physical Frames:
  Free/Used:   455 MiB / 0 B
  DMA pool:    116520 frames below 4 GiB, 0 above
  Operations:  0 allocs, 0 frees, 0 huge, 0 failures

Memory Regions:
  0x000000000000 - 0x000000001000  4.00 KiB  usable
  ...
  Kernel-owned: 0x00001d390000 - 0x00001e1cc000  14.2 MiB  kernel

Physical Addressing: 72 bits reported, 64 usable (max 0xffffffffffffffff)
SMBIOS Memory:  not exposed by firmware (no guest-reachable entry point)
```

"Firmware map" is not "RAM": a PCI MMIO hole covers most of the upper address
space, so `Allocatable` leads and the per-class breakdown reconciles the two.
See [memory.md](memory.md) for the full subsystem reference.

SMBIOS module details appear only when firmware exposes a guest-reachable
entry point, which plain OVMF does not.

### `date`
Display current date and time from RTC.

```bash
mfk> date
Friday, December 10, 2025
14:30:45 UTC
```

### `whoami`
Display current user (always "root").

```bash
mfk> whoami
root
```

### `test`
Run system diagnostics and tests.

```bash
mfk> test
Running tests...
✓ Test 1 passed
✓ Test 2 passed
...
```

## Calculation

### `calc <expression>`
Simple arithmetic calculator.

```bash
mfk> calc 5 + 3
Result: 8

mfk> calc 100 * 2
Result: 200

mfk> calc 10 / 2
Result: 5
```

Supports: `+`, `-`, `*`, `/`

## System Control

### `reboot`
Reboot the system.

```bash
mfk> reboot
Rebooting...
```

Triggers system reset via keyboard controller.

### `halt` / `shutdown`
Halt/shutdown the system.

```bash
mfk> halt
System halted. You can now turn off your computer.
```

System enters infinite halt loop. Press Ctrl+C or close QEMU.

## Network Commands

### `ifconfig [IP] [netmask] [gateway]`
Configure or display the E1000 interface. The netmask and gateway are optional;
the default netmask is `255.255.255.0`.

**QEMU user-mode NAT:**
```bash
mfk> ifconfig 10.0.2.15 255.255.255.0 10.0.2.2
```

### `ping <IP|hostname> [count]`
Send ICMP echo requests and wait for replies. The batch timeout is bounded at
five seconds; ARP failures and no-reply batches return to the prompt.

```bash
mfk> ping 10.0.2.2 4
Pinging 10.0.2.2 with 4 packets...
Reply from 10.0.2.2: seq=0 time=2ms
--- ping statistics ---
4 packets transmitted, 4 received, 0% packet loss
```

### `dns <hostname>`
Resolve an IPv4 hostname through the configured DNS path.

```bash
mfk> dns example.com
example.com -> 93.184.216.34
```

### `dhcp [start|renew|status]`
Obtain an address, mask, gateway and resolver from a DHCP server. `start` and
`renew` both run a full DORA; `status` reports the last negotiation.

```
mfk> dhcp
Requesting a DHCP lease...
DHCP: discover #1 (xid 0x3383a953)
DHCP: offer 10.0.2.15 from server 10.0.2.2
DHCP: request #1 for 10.0.2.15
IPv4 configured: 10.0.2.15 mask 255.255.255.0 gateway 10.0.2.2
DHCP: lease for 10.0.2.15 (mask 255.255.255.0, lease 86400s)
DHCP lease: 10.0.2.15 via 10.0.2.2
  netmask 255.255.255.0, lease 86400s
  resolver 10.0.2.3
```

The lease is validated before anything is configured: an offered address that is
`0.0.0.0`, broadcast, multicast or loopback, or that is the network or broadcast
address of the offered subnet, is refused, as is a missing or zero subnet mask, a
missing server identifier, or a `0.0.0.0` router. `status` reports how many
leases were refused and the reason for the last one.

The client sends before it has an address, so it does not use ARP: the first
frame goes out as an IPv4/UDP broadcast with a broadcast destination MAC. Renewal
works too — the server's reply is addressed to `255.255.255.255` even when the
host already holds a lease.

Note that the lease is obtained but not renewed on a timer, so a host left
running past the lease time will lose its address.

### `arp [-a|list|<IP>]`
Show the ARP cache, or resolve one IPv4 address with a bounded ARP wait.

```
mfk> arp -a
10.0.2.2  52:55:0a:00:02:02
10.0.2.3  52:55:0a:00:02:03  (stale)
```

A `(stale)` entry is past its TTL but still inside the grace window: usable so
existing connections are not cut, but re-resolved before it is trusted. The
table holds at most 64 entries and expires them itself, so an empty listing after
a long idle period is expected rather than a fault.

### `netstat`
Show the interface, address, netmask, gateway, ARP cache, and protocol state,
plus NIC interrupt, ARP, UDP queue, and TCP session counters. `-d` adds
per-packet tracing.

```
Protocol Stack:
  Ethernet - Active
  ARP      - Active
  IPv4     - Active
  ICMP     - Active
  UDP      - Active
  TCP      - Active
  DNS      - available
  UDP RX   - 0 queued (0 KiB of 256 KiB)
  IP       - 1 fragmented into 3 piece(s), 1 reassembled (0 held)
  ICMP     - 2 echo request(s), 1 reply(s), 0 error(s)
  NIC IRQ  - vector 43
            11 interrupts (8 tx, 9 tx underflow, last cause 0x3)
  ARP      - 1 req sent, 0 req recv, 0 reply sent, 1 reply recv
            0 evicted, 1 expired (table holds 64)
  TCP      - 0 active, 1 total
            7 seg sent (0 retransmitted), 5 received
            0 buffered, 0 in flight
```

`NIC IRQ` names the PIC vector the E1000 was assigned, or reports polling mode
when firmware gave it no INTx line. A transmit FIFO underflow count that grows
with bytes sent is normal for the emulated card; one growing faster would point
at a transmit-path bug. `last cause` is the raw cause register, useful when an
unmodelled bit appears.

The `ARP` lines appear only when something is worth reporting: a non-zero
spoof-rejection count means frames arrived claiming to be hosts they were not
sent by, and a rate-limit count means replies were suppressed by flood
protection. `evicted` counts entries lost to a full table — a network producing
more distinct senders than 64 — while `expired` is routine ageing.

`UDP RX` reports the byte budget, not just the datagram count, because a
datagram can be up to 64 KiB. A non-zero `dropped` line means datagrams were
discarded to stay inside 256 KiB; `bad checksum` means one arrived corrupt and
was refused rather than parsed.

The `IP` line reports fragmentation. A non-zero `fragmented into` means datagrams
were split to fit the link MTU; `reassembled` counts those put back together, and
the trailing number is partial datagrams currently held — it should return to
zero.

The `ICMP` line counts echo traffic and errors. `oversized echo request(s) not
reflected` means someone sent a ping with more than 64 bytes of payload and was
declined, which is the amplification guard refusing to reflect a large frame.

`TCP seg sent` versus `retransmitted` separates a clean transfer from one where
segments were lost and resent; `out of window` counts segments dropped as
duplicates or gaps. When connections are open, each is listed with its state,
buffered receive bytes, and unacknowledged transmit bytes.

### `tcpconnect`, `tcpsend`, `tcprecv`, `tcpclose`, `tcpstatus`, `tcpsockets`
Work with TCP connections. `tcpconnect` returns a **socket handle** such as
`socket#0g1`, and every other command takes that handle. The handle is the
connection's only identity: it is not a port number, and it does not survive
`tcpclose`.

```bash
mfk> tcpconnect 10.0.2.2 8080
Connecting to 10.0.2.2:8080...
Connection established: socket#0g1
  peer  10.0.2.2:8080
  local port 63058
mfk> tcpsend socket#0g1 GET /hello.txt HTTP/1.0\r\n\r\n
Sent 30 bytes on socket#0g1
Checking for response...
Received 141 bytes:
...
mfk> tcpclose socket#0g1
Closed socket#0g1 (peer 10.0.2.2:8080)
mfk> tcprecv socket#0g1
tcprecv: no such socket 'socket#0g1'
```

The `g<n>` part is a generation. Closing a socket retires its handle
permanently: when the slot is reused the newcomer has a higher generation, so
`tcpsend socket#0g1` after a close cannot accidentally address the new
connection. A bare index (`tcpsend 0`) also works and means "whatever socket is
in slot 0 now".

`tcpsend` expands `\r`, `\n`, `\t`, `\0` and `\\` in its data argument, because a
terminal cannot type a line terminator into an argument and a text protocol
needs one. Unknown escapes such as `\q` are passed through unchanged, so a
Windows path survives. Note that `\n` and `\t` *are* escapes, so a payload
containing them is altered.

`tcpsend` reads the reply for you; use `tcprecv <socket> [seconds]` when you
want to wait for more. `tcprecv` reports a closed peer as end of stream, and
times out otherwise.

`tcpsockets` lists open sockets with transport, local port, peer and state — use
it if you have lost track of a handle.

### `udp-send` / `udp-recv`
Send and receive diagnostic UDP datagrams.

```bash
mfk> udp-send 10.0.2.2 49153 5555 hello
mfk> udp-recv 5555 5
```

`udp-send` takes its payload as a single word, so it cannot reach past the link
MTU. Use `udp-ping` for that.

### `udp-ping <IPv4|hostname> <bytes> [remote-port]`
Send a UDP datagram of a given size, filled with a repeating non-zero pattern so
a reassembled datagram can be checked for corruption rather than merely counted.
Refuses more than 60000 bytes.

```bash
mfk> udp-ping 10.0.2.2 3000
Sent 3000 UDP payload bytes to 10.0.2.2:5555
mfk> netstat
  IP       - 1 fragmented into 3 piece(s), 1 reassembled (0 held)
```

This is the way to exercise IPv4 fragmentation from the shell. A 3000-byte
datagram does not fit a 1500-byte link, so it is split into three pieces and put
back together at the receiver.

### `wget <http(s)-url> <file>`
Download an HTTP(S) response to the mounted SimplFS filesystem. TLS is
enabled by default; build with `--no-tls` to omit it (HTTPS then reports
that TLS is not enabled).

### `speedtest` / `speedtest-server`
Run a LibreSpeed-compatible test or display/set its server URL.

### `netdebug [on|off|status]`
Enable or disable gated per-packet serial diagnostics. `wget` and `speedtest`
also accept `-d` or `--debug` for one invocation.

### `tlsinfo`
Show whether the TLS 1.3 backend is compiled into the kernel (on by
default; `--no-tls` builds the stub instead).

## File System Commands

### `diskinfo`
Display disk information.

```bash
mfk> diskinfo
Disk Information:
  Drive 0 (Primary Master): 2880 blocks
  Drive 1 (Primary Slave): 2880 blocks
  ...
```

### `mkfs`
Format disk with SimpleFS filesystem. Only SimplFS can be created by MFK
itself; ext4/exFAT drivers mount existing images (created by host tools)
but intentionally do not format fresh ones.

```bash
mfk> mkfs
Formatting disk...
Filesystem created and mounted
```

Creates inode table and initializes filesystem structures.

### `mount`
Mount a filesystem. With no format named, the device is probed (SimplFS,
then ext4, then exFAT) and whatever is found is mounted; unknown content is
refused rather than blind-mounted.

```bash
mfk> mount
simplfs filesystem mounted successfully from drive 1!
mfk> mount 4 exfat
exfat filesystem mounted successfully from drive 4!
```

Must run after `mkfs` or on an existing formatted disk. Mounting validates
the on-disk geometry before handing back a filesystem. Only one filesystem
is active at a time; mounting a new drive replaces the current mount.

`mount` refuses a **version 1** SimplFS image: version 1 kept free-space
state only in RAM and reconstructed it by walking every inode, so a block
whose owning inode record had not reached the platter was handed out a
second time. Run `mkfs` to reformat.

ext4 images mount read-write only when cleanly unmounted (no journal
recovery pending); otherwise the mount is refused for writing. Images with
`metadata_csum`, inline data, encryption or casefold are refused outright
rather than risk silent corruption.

### `umount`
Unmount the active filesystem.

```bash
mfk> umount
Filesystem unmounted.
```

### `stat <path>`
Show file metadata (type, size, link count, owner, mode, timestamps).

```bash
mfk> stat /hello.txt
  File: /hello.txt
  Type: file  Size: 15  Links: 1
```

### `df`
Show the mounted filesystem type, mount drive, and block-cache hit rate.

### `cp <src> <dst>` / `mv <src> <dst>`
Copy a file, or move/rename a file or directory. A trailing slash on the
destination keeps the source file name (`cp a.txt backup/`).

### `ln [-s] <target> <link>`
Hard link or symbolic link. Only filesystems with link support (ext4)
accept these; SimplFS and exFAT report `Unsupported`.

### `fsck`
Check filesystem consistency. Read-only: nothing on disk is modified.

```bash
mfk> fsck -v
Filesystem check (drive 1):
  Total blocks:   262144 (262007 data, first at LBA 137)
  Bitmap:         LBA 73, 64 block(s)
  Inodes:         3 in use across 72 block(s)
  Free blocks:    262004 reported, 262004 in bitmap

  Geometry:       ok
  Block counts:   consistent
  Block pointers: all in range
  Indirect chains: all terminate
  Dir entries:    all resolve
  Space refs:     all accounted
  Leaked space:   none

Filesystem is clean.
```

Each line is a class of on-disk damage:

| Check | Detects |
|---|---|
| Geometry | Superblock fields that contradict each other |
| Block counts | `free_blocks` disagreeing with the allocation bitmap |
| Block pointers | Block references outside the data region |
| Indirect chains | Chains that do not terminate (cycles) |
| Dir entries | Entries naming a non-existent or out-of-range inode |
| Space refs | Blocks an inode references that the bitmap calls free |
| Leaked space | Blocks marked used that no inode references |

The last two are only meaningful because version 2 persists the allocation
bitmap: with free-space state derived from the inodes, both sets were the same
by construction and the comparison could never find anything.

### `ls` / `dir`
List files in current directory.

```bash
mfk> ls
Files:
  file1.txt (512 bytes)
  data.bin (1024 bytes)
  readme.txt (256 bytes)
```

### `touch <filename>`
Create an empty file.

```bash
mfk> touch newfile.txt
File created: newfile.txt
```

### `write <filename> <content>`
Create or overwrite a file with content.

```bash
mfk> write test.txt "Hello, World!"
File written: test.txt (13 bytes)

mfk> write data.bin This_is_longer_content
File written: data.bin (25 bytes)
```

### `cat <filename>`
Display file contents.

```bash
mfk> cat test.txt
Hello, World!
```

For binary files, displays raw bytes.

### `rm <filename>`
Delete a file.

```bash
mfk> rm test.txt
File deleted: test.txt
```

## Archive Commands

All five archivers share the same shape: create (`c`), list (`t`), extract
(`x`) and integrity-test, plus `-v` for verbose output on every mode. Member
paths are always rewritten relative to the destination and `..`/absolute
paths are refused, so an archive can never write outside the destination.

Because the kernel heap is 16 MiB, every decoder is capped at
`MAX_DECOMPRESSED_BYTES` (4 MiB) of decoded output; a larger archive fails
with `archive too large` instead of exhausting the heap.

| Command | Container | Notes |
| --- | --- | --- |
| `tar` | tar + wrappers | wrapper chosen by output extension |
| `zip` / `unzip` | ZIP | deflate by default, `-0` stores |
| `7z` | 7z | Stored / LZMA1 / LZMA2 |
| `mfk` | native MFK | per-member CRC32, stored or deflate |

### `tar -c|-t|-x [-v] [-z|-J] -f <archive> [operands...]`
Create, list or extract a tarball. `-z` forces gzip and `-J` forces xz;
otherwise the output name decides the wrapper.

```bash
mfk> tar -cf backup.tar notes.txt src
mfk> tar -czf backup.tar.gz notes.txt src     # force gzip
mfk> tar -cJf backup.tar.xz notes.txt         # force xz
mfk> tar -tvf backup.tar.gz                  # verbose listing
mfk> tar -xf backup.tar.zst -C dest          # extract into dest/
```

| Mode | Meaning |
| --- | --- |
| `-c` | create from the operands (files and directories, walked recursively) |
| `-t` | list members without extracting |
| `-x` | extract, creating parent directories as needed |
| `-v` | verbose: sizes for `t`, one line per member for `x` |

Tar flavours read: v7/ustar, GNU long names (`L`), and PAX `path`/`size`
records. Symlinks, hardlinks, device nodes and sparse files are skipped
rather than mis-restored.

| Output name | Wrapper | Read | Create |
| --- | --- | --- | --- |
| `.tar`, no extension | none | yes | yes |
| `.tar.gz`, `.tgz` | gzip | yes | yes |
| `.tar.xz`, `.txz` | xz | yes | yes |
| `.tar.lz4`, `.tlz4` | LZ4 frame | yes | yes |
| `.tar.zst`, `.tzst` | zstandard | yes | yes |
| `.tar.lz` | lzip | yes | yes |
| `.tar.lzma`, `.lzma` | legacy LZMA1 | yes | yes |
| `.tar.bz2`, `.tbz2`, `.tbz` | bzip2 | yes | no |
| `.tar.Z`, `.Z` | legacy `compress` (LZW) | yes | no |

The last two are decode-only: bzip2 has no no_std encoder available here and
the LZW compressor is deliberately not implemented, so both refuse creation
with a message pointing at host-side compression.

### `zip [-0|-6|-9] [-v] <archive.zip> <FILE...>` / `unzip [-v] <archive.zip> [-d dir]`
Create and read ZIP archives. `-0` stores without deflating; `-6`/`-9` select
deflate (level 6 is what this implementation emits).

```bash
mfk> zip out.zip notes.txt
mfk> zip -0v out.zip notes.txt      # stored, verbose
mfk> unzip -v out.zip
mfk> unzip out.zip -d dest
```

### `7z a|t|x <archive.7z> [FILE...]` / `7z l|t <archive.7z>`
Create, list, test and extract 7z archives. Creation always uses LZMA2
(preset 6), which is what 7-Zip picks by default.

```bash
mfk> 7z a out.7z notes.txt src
mfk> 7z l out.7z
mfk> 7z t out.7z                   # verify CRCs without extracting
mfk> 7z x out.7z -odest            # extract into dest/
```

The reader handles stored, LZMA1 and LZMA2 folders, including solid
folders with multiple sub-streams and compressed (encoded) headers. Folders
that use AES encryption, PPMd, bzip2, Brotli, zstd, LZ4 or BCJ filters are
refused with `unsupported codec` rather than decoded incorrectly.

### `mfk c|t|x <archive.mfk> [FILE...]` / `mfk l <archive.mfk>`
The native container. Each member stores its own CRC32, so a corrupt member
is named instead of silently yielding garbage.

```bash
mfk> mfk c out.mfk notes.txt src
mfk> mfk l -v out.mfk
mfk> mfk t out.mfk
mfk> mfk x out.mfk -d dest
```

Members are stored verbatim or DEFLATE-compressed per entry; `-v` on a
listing prints the compressed size, uncompressed size and method.

## Command Examples

### System Exploration
```bash
mfk> about              # What is MFK?
mfk> help               # Available commands
mfk> cpuinfo            # CPU vendor and model
mfk> memory             # Memory map
mfk> uptime             # How long running?
mfk> date               # Current date/time
```

### Network Testing
```bash
mfk> ifconfig           # Check network config
mfk> netstat            # Network status
mfk> ping 10.0.2.2 4    # Ping gateway
mfk> ping 8.8.8.8       # Ping external (may not work)
```

### File Operations
```bash
mfk> mkfs               # Create filesystem
mfk> mount              # Mount filesystem
mfk> ls                 # List files
mfk> write test.txt "content"  # Create file
mfk> cat test.txt       # Read file
mfk> rm test.txt        # Delete file
```

### System Control
```bash
mfk> color green        # Change color
mfk> clear              # Clear screen
mfk> uptime             # Show uptime
mfk> calc 5 + 3         # Calculate
mfk> halt               # Shutdown
```

## Command Tips

- **Case-sensitive**: Commands are case-sensitive (`Help` ≠ `help`)
- **Whitespace**: Commands separated by spaces
- **No pipes**: No redirection or piping (not implemented)
- **Line editing**: Backspace to delete, no arrow keys
- **Interrupt**: Ctrl+C may cancel long operations
- **Short forms**: Some commands have aliases (`cls` = `clear`, `mem` = `memory`)

## Error Handling

If a command fails:

```bash
mfk> ping invalid
Invalid IP address format

mfk> cat nonexistent.txt
File not found

mfk> unknown_command
Unknown command: 'unknown_command'. Type 'help' for available commands.
```

## Future Commands

Planned but not yet implemented:
- `cat` with multiple files
- `grep` for file searching
- `mkdir` for directory creation
- `cd` for directory navigation
- `tcp` for TCP connection testing
- `dhcp` for automatic IP configuration
- `dns` for domain resolution

## Next Steps

- **[Network Stack](networking.md)** — How networking works
- **[File System](filesystem.md)** — File storage details
- **[Running the Kernel](../guide/running.md)** — Boot and use
